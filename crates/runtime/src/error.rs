//! Evaluation errors.

use std::fmt;
use std::ops::Range;
use std::rc::Rc;

/// An evaluation error, with Nix's wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub message: String,
    /// Where the expression that failed is, if known. The runtime itself
    /// knows no locations; the evaluator attaches them.
    pub origin: Option<Origin>,
}

/// The source range of an expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// The name of the source (a file's absolute path, or `«string»`).
    pub source: Rc<str>,
    pub span: Range<usize>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Error {
            message: message.into(),
            origin: None,
        }
    }

    /// Attaches `origin` unless the error already has a (more precise) one.
    pub fn at(mut self, origin: impl FnOnce() -> Option<Origin>) -> Self {
        if self.origin.is_none() {
            self.origin = origin();
        }
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}
