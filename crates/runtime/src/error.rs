//! Evaluation errors.

use std::fmt;
use std::ops::Range;

/// An evaluation error, with Nix's wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub message: String,
    /// The source range of the expression that failed, if known. The runtime
    /// itself knows no locations; the evaluator attaches them.
    pub span: Option<Range<usize>>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Error {
            message: message.into(),
            span: None,
        }
    }

    /// Attaches `span` unless the error already has a (more precise) one.
    pub fn at(mut self, span: Option<Range<usize>>) -> Self {
        if self.span.is_none() {
            self.span = span;
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
