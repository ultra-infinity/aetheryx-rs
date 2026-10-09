//! Guarding against stack exhaustion.
//!
//! Evaluation recurses as deeply as the Nix program does. Where Nix would
//! report `stack overflow (possible infinite recursion)` instead of crashing,
//! so does this runtime: before recursing it checks that at least
//! [`RED_ZONE`] bytes of stack are left. The cut-off depth differs from Nix's,
//! which depends on its own stack frames; the behaviour (an error, never a
//! crash) is the same.

use crate::error::{Error, Result};

/// How much stack must remain before recursing further.
pub const RED_ZONE: usize = 1024 * 1024;

/// Nix's message when the stack runs out.
pub const STACK_OVERFLOW: &str = "stack overflow (possible infinite recursion)";

/// Fails with Nix's stack overflow error if the stack is nearly exhausted.
pub fn check() -> Result<()> {
    match stacker::remaining_stack() {
        Some(remaining) if remaining < RED_ZONE => Err(Error::new(STACK_OVERFLOW)),
        _ => Ok(()),
    }
}
