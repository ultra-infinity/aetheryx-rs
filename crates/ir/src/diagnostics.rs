//! Source locations and diagnostics.
//!
//! Every op produced by [`crate::ast_to_hir`] carries the location of the Nix
//! source it came from, so later stages can still report errors against the
//! original source after the AST is gone. A location is encoded as
//!
//! ```text
//! fused<nix_loc.span 10..14>["/abs/file.nix": line: 1, column: 11]
//! ```
//!
//! i.e. a pliron [`Location::Fused`] whose metadata is a [`SpanAttr`] with the
//! exact byte range, wrapping a human readable line/column position.
//! [`span_of`] recovers the byte range from such a location.
//!
//! Errors are reported as [`SourceError`]s, which render through `miette`
//! with the offending source snippet.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use miette::{Diagnostic, LabeledSpan, NamedSource, SourceCode, SourceSpan};
use pliron::combine::stream::position::SourcePosition;
use pliron::context::Context;
use pliron::derive::pliron_attr;
use pliron::location::{Location, Source};
use pliron::uniqued_any;
use rnix::{ParseError, SyntaxKind, TextRange, TextSize};
use thiserror::Error;

use crate::paths;

/// Byte range `[start, end)` in the source file of a located op.
#[pliron_attr(name = "nix_loc.span", format = "$start `..` $end", verifier = "succ")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpanAttr {
    pub start: u32,
    pub end: u32,
}

/// Recovers the byte span stored in a location created by [`SourceFile::location`].
pub fn span_of(loc: &Location) -> Option<SourceSpan> {
    match loc {
        Location::Fused {
            metadata: Some(metadata),
            ..
        } => metadata
            .downcast_ref::<SpanAttr>()
            .map(|s| SourceSpan::from(s.start as usize..s.end as usize)),
        Location::Named { child_loc, .. } => span_of(child_loc),
        _ => None,
    }
}

/// The position of a location created by [`SourceFile::location`], as Nix
/// prints positions: `/abs/file.nix:3:7`, or `«string»:3:7`.
pub fn position_of(ctx: &Context, loc: &Location) -> Option<String> {
    match loc {
        Location::Fused { locations, .. } => locations.iter().find_map(|l| position_of(ctx, l)),
        Location::SrcPos {
            src: Source::File(file),
            pos,
        } => Some(format!(
            "{}:{}:{}",
            uniqued_any::get(ctx, *file).display(),
            pos.line,
            pos.column
        )),
        Location::Named { child_loc, .. } => position_of(ctx, child_loc),
        _ => None,
    }
}

/// The name of the source of a location created by [`SourceFile::location`]
/// (what [`SourceFile::abs_path`] returns).
pub fn source_name_of(ctx: &Context, loc: &Location) -> Option<String> {
    match loc {
        Location::Fused { locations, .. } => locations.iter().find_map(|l| source_name_of(ctx, l)),
        Location::SrcPos {
            src: Source::File(file),
            ..
        } => Some(uniqued_any::get(ctx, *file).display().to_string()),
        Location::Named { child_loc, .. } => source_name_of(ctx, child_loc),
        _ => None,
    }
}

/// The name Nix gives the source of an `--expr` expression.
pub const EXPR_NAME: &str = "«string»";

/// The name Nix gives the source of an expression read from standard input.
pub const STDIN_NAME: &str = "«stdin»";

/// A Nix source being compiled: a file, or an expression given on the
/// command line.
pub struct SourceFile {
    /// The path as given, used when displaying diagnostics.
    path: PathBuf,
    /// For a file, its path made absolute and normalized the way Nix does it
    /// (against the current directory, without resolving symlinks); for an
    /// expression, [`EXPR_NAME`]. Positions show this name.
    abs_path: String,
    /// The directory relative path literals are resolved against.
    base_dir: String,
    /// Whether this is an expression rather than a file.
    is_expr: bool,
    /// The file's text, shared with the errors reported against it.
    source: Arc<NamedSource<String>>,
    src: Source,
    /// Byte offset of the start of each line.
    line_starts: Vec<usize>,
}

impl SourceFile {
    /// A file at `path`, made absolute against the current directory.
    pub fn new(
        ctx: &Context,
        path: impl Into<PathBuf>,
        text: impl Into<String>,
    ) -> std::io::Result<Self> {
        Self::with_cwd(ctx, path, text, &std::env::current_dir()?)
    }

    /// A file at `path`, made absolute against `cwd`.
    ///
    /// Fails if either path is not valid UTF-8.
    pub fn with_cwd(
        ctx: &Context,
        path: impl Into<PathBuf>,
        text: impl Into<String>,
        cwd: &Path,
    ) -> std::io::Result<Self> {
        let path = path.into();
        let text = text.into();
        let utf8 = |p: &Path| {
            p.to_str().map(str::to_owned).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("path is not valid UTF-8: {}", p.display()),
                )
            })
        };
        let abs_path = paths::abs_path(&utf8(&path)?, &utf8(cwd)?);
        let base_dir = paths::dir_of(&abs_path).to_string();
        Ok(Self::build(ctx, path, abs_path, base_dir, false, text))
    }

    /// An expression given on the command line (`--expr`), whose relative
    /// paths resolve against `cwd`. Like Nix, its positions show
    /// [`EXPR_NAME`], and `__curPos` in it is `null`.
    ///
    /// Fails if `cwd` is not valid UTF-8.
    pub fn expr(ctx: &Context, text: impl Into<String>, cwd: &Path) -> std::io::Result<Self> {
        Self::unnamed(ctx, EXPR_NAME, text.into(), cwd)
    }

    /// An expression read from standard input (`--file -`): like
    /// [`Self::expr`], but its positions show [`STDIN_NAME`].
    pub fn stdin(ctx: &Context, text: impl Into<String>, cwd: &Path) -> std::io::Result<Self> {
        Self::unnamed(ctx, STDIN_NAME, text.into(), cwd)
    }

    fn unnamed(ctx: &Context, name: &str, text: String, cwd: &Path) -> std::io::Result<Self> {
        let cwd = cwd.to_str().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("path is not valid UTF-8: {}", cwd.display()),
            )
        })?;
        let base_dir = paths::abs_path(".", cwd);
        Ok(Self::build(
            ctx,
            name.into(),
            name.to_string(),
            base_dir,
            true,
            text,
        ))
    }

    fn build(
        ctx: &Context,
        path: PathBuf,
        abs_path: String,
        base_dir: String,
        is_expr: bool,
        text: String,
    ) -> Self {
        let line_starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        let src = Source::new_from_file(ctx, abs_path.clone());
        let source = Arc::new(NamedSource::new(path.display().to_string(), text));
        SourceFile {
            path,
            abs_path,
            base_dir,
            is_expr,
            source,
            src,
            line_starts,
        }
    }

    /// The path as given.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The name positions show: the normalized absolute path of a file, or
    /// [`EXPR_NAME`].
    pub fn abs_path(&self) -> &str {
        &self.abs_path
    }

    /// The file `__curPos` reports, or `None` for an expression, where Nix
    /// makes `__curPos` `null`.
    pub fn cur_pos_file(&self) -> Option<&str> {
        (!self.is_expr).then_some(self.abs_path.as_str())
    }

    /// The directory that relative path literals are resolved against.
    pub fn base_dir(&self) -> &str {
        &self.base_dir
    }

    pub fn text(&self) -> &str {
        self.source.inner()
    }

    /// 1-based line and column of a byte offset. Like Nix, the column counts
    /// bytes, so `é` (two bytes in UTF-8) advances it by 2.
    pub fn line_col(&self, offset: TextSize) -> (usize, usize) {
        let offset = usize::from(offset);
        let line = self.line_starts.partition_point(|&s| s <= offset) - 1;
        (line + 1, offset - self.line_starts[line] + 1)
    }

    /// A position as Nix prints it in messages: `/abs/file.nix:line:column`.
    pub fn position(&self, offset: TextSize) -> String {
        let (line, column) = self.line_col(offset);
        format!("{}:{line}:{column}", self.abs_path)
    }

    /// The IR location of `range`.
    pub fn location(&self, range: TextRange) -> Location {
        let (line, column) = self.line_col(range.start());
        Location::Fused {
            metadata: Some(Box::new(SpanAttr {
                start: range.start().into(),
                end: range.end().into(),
            })),
            locations: vec![Location::SrcPos {
                src: self.src,
                pos: SourcePosition {
                    line: line as i32,
                    column: column as i32,
                },
            }],
        }
    }

    /// The range covering the whole file.
    pub fn full_range(&self) -> TextRange {
        TextRange::up_to(TextSize::of(self.text()))
    }

    /// Attaches this file's source to a diagnostic.
    /// An error in this file at the byte range `span`, if known.
    pub fn error_at(&self, kind: ErrorKind, span: Option<std::ops::Range<usize>>) -> SourceError {
        self.error(Diag {
            kind,
            span: span.map(SourceSpan::from),
            secondary: Vec::new(),
        })
    }

    pub(crate) fn error(&self, diag: Diag) -> SourceError {
        SourceError {
            kind: diag.kind,
            src: Arc::clone(&self.source),
            span: diag.span,
            secondary: diag.secondary,
        }
    }

    /// Converts rnix parse errors into diagnostics.
    pub fn check_parse(&self, errors: &[ParseError]) -> Result<(), ParseErrors> {
        if errors.is_empty() {
            return Ok(());
        }
        let eof = TextRange::empty(TextSize::of(self.text()));
        let is_eof = |err: &ParseError| {
            matches!(
                err,
                ParseError::UnexpectedEOF | ParseError::UnexpectedEOFWanted(_)
            )
        };
        // After a real error, rnix's recovery tends to run into the end of the
        // file and report that too; those follow-up errors are just noise.
        let has_real_error = errors.iter().any(|e| !is_eof(e));
        let mut seen = Vec::new();
        let errors = errors
            .iter()
            .filter(|err| !(has_real_error && is_eof(err)))
            .filter(|err| {
                let key = err.to_string();
                let new = !seen.contains(&key);
                seen.push(key);
                new
            })
            .map(|err| {
                let (msg, span) = match err {
                    ParseError::Unexpected(r) => ("unexpected token".to_string(), Some(*r)),
                    ParseError::UnexpectedExtra(r) => (
                        "unexpected token after the end of the expression".to_string(),
                        Some(*r),
                    ),
                    ParseError::UnexpectedWanted(found, r, wanted) => (
                        format!(
                            "unexpected {}, expected {}",
                            kind_name(*found),
                            one_of(wanted)
                        ),
                        Some(*r),
                    ),
                    ParseError::UnexpectedDoubleBind(r) => {
                        ("pattern is bound twice".to_string(), Some(*r))
                    }
                    ParseError::UnexpectedEOF => ("unexpected end of file".to_string(), Some(eof)),
                    ParseError::UnexpectedEOFWanted(wanted) => (
                        format!("unexpected end of file, expected {}", one_of(wanted)),
                        Some(eof),
                    ),
                    ParseError::DuplicatedArgs(r, name) => (
                        format!("duplicate formal function argument '{name}'"),
                        Some(*r),
                    ),
                    ParseError::RecursionLimitExceeded => {
                        ("expression is nested too deeply".to_string(), None)
                    }
                    _ => (err.to_string(), None),
                };
                self.error(Diag {
                    kind: ErrorKind::Parse(msg),
                    span: span.map(to_span),
                    secondary: Vec::new(),
                })
            })
            .collect();
        Err(ParseErrors {
            path: self.path.display().to_string(),
            errors,
        })
    }
}

fn to_span(range: TextRange) -> SourceSpan {
    SourceSpan::from(usize::from(range.start())..usize::from(range.end()))
}

/// `TOKEN_SEMICOLON` -> `semicolon`.
fn kind_name(kind: SyntaxKind) -> String {
    let name = format!("{kind:?}");
    name.trim_start_matches("TOKEN_")
        .trim_start_matches("NODE_")
        .to_lowercase()
        .replace('_', " ")
}

fn one_of(kinds: &[SyntaxKind]) -> String {
    match kinds {
        [one] => kind_name(*one),
        _ => format!(
            "one of: {}",
            kinds
                .iter()
                .map(|k| kind_name(*k))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// What went wrong.
#[derive(Debug, Clone, Error)]
pub enum ErrorKind {
    #[error("{0}")]
    Parse(String),
    #[error("syntax error")]
    Syntax,
    #[error("missing {0}")]
    Missing(&'static str),
    /// `position` is where the attribute was first defined.
    #[error("attribute '{name}' already defined at {position}")]
    DuplicateAttr { name: String, position: String },
    #[error("duplicate formal function argument '{0}'")]
    DuplicateFormal(String),
    #[error("invalid integer '{0}'")]
    InvalidInteger(String),
    #[error("invalid float '{0}'")]
    InvalidFloat(String),
    #[error("undefined variable '{0}'")]
    UndefinedVariable(String),
    #[error("dynamic attributes not allowed in {0}")]
    DynamicAttrNotAllowed(&'static str),
    #[error("cannot resolve '{0}': the home directory is unknown")]
    NoHomeDir(String),
    /// The input nests too deeply for the remaining stack.
    #[error("stack overflow (possible infinite recursion)")]
    StackOverflow,
    /// An evaluation error; the message is Nix's.
    #[error("{0}")]
    Eval(String),
    #[error("IR verification failed: {0}")]
    Verify(String),
}

impl ErrorKind {
    fn code(&self) -> &'static str {
        match self {
            ErrorKind::Parse(_) => "nix::parse",
            ErrorKind::Syntax => "nix::syntax",
            ErrorKind::Missing(_) => "nix::missing",
            ErrorKind::DuplicateAttr { .. } => "nix::duplicate_attr",
            ErrorKind::DuplicateFormal(_) => "nix::duplicate_formal",
            ErrorKind::InvalidInteger(_) => "nix::invalid_integer",
            ErrorKind::InvalidFloat(_) => "nix::invalid_float",
            ErrorKind::UndefinedVariable(_) => "nix::undefined_variable",
            ErrorKind::DynamicAttrNotAllowed(_) => "nix::dynamic_attr",
            ErrorKind::NoHomeDir(_) => "nix::no_home_dir",
            ErrorKind::StackOverflow => "nix::stack_overflow",
            ErrorKind::Eval(_) => "nix::eval",
            ErrorKind::Verify(_) => "nix::ir::verify",
        }
    }

    fn help(&self) -> Option<&'static str> {
        match self {
            ErrorKind::DuplicateAttr { .. } => Some(
                "attribute paths like `a.b = ...; a.c = ...;` are merged, \
                 but plain values cannot be",
            ),
            ErrorKind::DynamicAttrNotAllowed(_) => Some("use a static name here"),
            ErrorKind::Verify(_) => Some("this is a bug in the compiler"),
            _ => None,
        }
    }
}

/// A diagnostic before the source text is attached to it.
pub(crate) struct Diag {
    pub kind: ErrorKind,
    pub span: Option<SourceSpan>,
    pub secondary: Vec<(SourceSpan, String)>,
}

impl Diag {
    pub fn new(kind: ErrorKind, range: TextRange) -> Self {
        Diag {
            kind,
            span: Some(to_span(range)),
            secondary: Vec::new(),
        }
    }

    pub fn with_label(mut self, range: TextRange, label: impl Into<String>) -> Self {
        self.secondary.push((to_span(range), label.into()));
        self
    }

    /// A diagnostic located by an IR location (see [`span_of`]).
    pub fn at_location(kind: ErrorKind, loc: &Location) -> Self {
        Diag {
            kind,
            span: span_of(loc),
            secondary: Vec::new(),
        }
    }
}

/// An error in a Nix source file.
#[derive(Debug, Error)]
#[error("{kind}")]
pub struct SourceError {
    pub kind: ErrorKind,
    src: Arc<NamedSource<String>>,
    span: Option<SourceSpan>,
    secondary: Vec<(SourceSpan, String)>,
}

impl SourceError {
    pub fn span(&self) -> Option<SourceSpan> {
        self.span
    }
}

impl Diagnostic for SourceError {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new(self.kind.code()))
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.kind
            .help()
            .map(|h| Box::new(h) as Box<dyn std::fmt::Display>)
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        Some(&self.src)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let primary = self
            .span
            .map(|s| LabeledSpan::new_primary_with_span(Some("here".to_string()), s));
        let secondary = self
            .secondary
            .iter()
            .map(|(s, l)| LabeledSpan::new_with_span(Some(l.clone()), *s));
        Some(Box::new(primary.into_iter().chain(secondary)))
    }
}

/// All parse errors of one file.
#[derive(Debug, Error, Diagnostic)]
#[error("failed to parse {path}")]
#[diagnostic(code(nix::parse))]
pub struct ParseErrors {
    path: String,
    #[related]
    pub errors: Vec<SourceError>,
}
