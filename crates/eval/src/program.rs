//! Compiling sources together and evaluating them: the file or expression
//! being evaluated, and the expressions given with it (`--arg`, `--apply`).

use std::path::{Path, PathBuf};
use std::rc::Rc;

use ir::ast_to_hir::{LowerOptions, lower_root_with};
use ir::diagnostics::{ErrorKind, SourceFile, span_of};
use ir::hir_to_mir::lower_module;
use pliron::builtin::ops::ModuleOp;
use pliron::context::Context;
use runtime::{Builtins, Error, Value};

use crate::{EvalError, Interp};

/// Sources being compiled into one program.
pub struct Program {
    ctx: Context,
    options: LowerOptions,
    sources: Vec<(SourceFile, ModuleOp)>,
}

/// A source added to a [`Program`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceId(usize);

impl Program {
    pub fn new(options: LowerOptions) -> Self {
        Program {
            ctx: Context::new(),
            options,
            sources: Vec::new(),
        }
    }

    /// Compiles the file at `path` (relative to `cwd`), whose contents are
    /// `text`.
    pub fn add_file(
        &mut self,
        path: impl Into<PathBuf>,
        text: impl Into<String>,
        cwd: &Path,
    ) -> Result<SourceId, EvalError> {
        let file = SourceFile::with_cwd(&self.ctx, path, text, cwd)
            .map_err(|e| EvalError::Message(e.to_string()))?;
        self.add(file)
    }

    /// Compiles an expression given on the command line, whose relative
    /// paths resolve against `cwd`.
    pub fn add_expr(&mut self, text: impl Into<String>, cwd: &Path) -> Result<SourceId, EvalError> {
        let file = SourceFile::expr(&self.ctx, text, cwd)
            .map_err(|e| EvalError::Message(e.to_string()))?;
        self.add(file)
    }

    /// Compiles an expression read from standard input, whose relative paths
    /// resolve against `cwd`.
    pub fn add_stdin(
        &mut self,
        text: impl Into<String>,
        cwd: &Path,
    ) -> Result<SourceId, EvalError> {
        let file = SourceFile::stdin(&self.ctx, text, cwd)
            .map_err(|e| EvalError::Message(e.to_string()))?;
        self.add(file)
    }

    fn add(&mut self, file: SourceFile) -> Result<SourceId, EvalError> {
        let parse = ir::stack::parse(file.text());
        file.check_parse(parse.errors())?;
        let module = lower_root_with(&mut self.ctx, &file, &parse.tree(), &self.options)?;
        lower_module(&mut self.ctx, module).map_err(|e| {
            let span = span_of(&e.loc).map(|s| s.offset()..s.offset() + s.len());
            file.error_at(ErrorKind::Verify(e.err.to_string()), span)
        })?;
        self.sources.push((file, module));
        Ok(SourceId(self.sources.len() - 1))
    }

    /// Ends compilation; the sources can now be evaluated.
    pub fn start(self) -> Session {
        Session {
            interp: Rc::new(Interp::new(
                Rc::new(self.ctx),
                Builtins::new(self.options.pure_eval),
            )),
            sources: Rc::new(self.sources),
        }
    }
}

/// The compiled sources of a [`Program`], being evaluated.
pub struct Session {
    interp: Rc<Interp>,
    sources: Rc<Vec<(SourceFile, ModuleOp)>>,
}

impl Session {
    /// The value of the source `id`, not evaluated yet.
    ///
    /// Nix calls may nest [`runtime::ops::MAX_CALL_DEPTH`] deep, and the
    /// interpreter recurses with them: force values on a thread with a large
    /// stack (the driver uses 256 MiB).
    pub fn value(&self, id: SourceId) -> Value {
        let interp = Rc::clone(&self.interp);
        let module = self.sources[id.0].1;
        Value::thunk(move || interp.run_main(module))
    }

    /// Reports `error` against the source it happened in, if that is known.
    pub fn error(&self, error: Error) -> EvalError {
        let file = error.origin.as_ref().and_then(|origin| {
            let mut named = self
                .sources
                .iter()
                .filter(|(file, _)| file.abs_path() == &*origin.source);
            // Expressions all share the name `«string»`; without knowing
            // which one failed, show no source.
            match (named.next(), named.next()) {
                (Some((file, _)), None) => Some((file, origin.span.clone())),
                _ => None,
            }
        });
        match file {
            Some((file, span)) => file
                .error_at(ErrorKind::Eval(error.message), Some(span))
                .into(),
            None => EvalError::Message(error.message),
        }
    }
}
