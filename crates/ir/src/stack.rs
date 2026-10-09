//! Running deeply recursive code without overflowing the stack.
//!
//! Syntax trees and IR nest as deeply as the source does. Steps that can fail
//! report Nix's "stack overflow" error when the stack runs low (see
//! [`ErrorKind::StackOverflow`](crate::diagnostics::ErrorKind::StackOverflow));
//! steps that cannot fail use [`grow`] instead.

use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};

/// How much stack must remain before recursing further.
pub const RED_ZONE: usize = 1024 * 1024;

/// The size of each extra stack segment [`grow`] allocates.
const SEGMENT: usize = 64 * 1024 * 1024;

/// The stack for recursion inside code we don't control (rnix's parser and
/// the drop of its trees), which cannot check the stack as it goes. It is
/// reserved virtual memory, committed only as far as it is used.
const FOREIGN_STACK: usize = 1024 * 1024 * 1024;

/// Whether less than [`RED_ZONE`] of stack is left.
pub fn is_low() -> bool {
    stacker::remaining_stack().is_some_and(|remaining| remaining < RED_ZONE)
}

/// Runs `f`, on a newly allocated stack segment if the current one is low.
///
/// This checks the stack once, so `f` must recurse through `grow` again (as
/// this crate's passes do) rather than recursing deeply on its own.
pub fn grow<T>(f: impl FnOnce() -> T) -> T {
    stacker::maybe_grow(RED_ZONE, SEGMENT, f)
}

/// Runs foreign code that may recurse deeply on its own, on a dedicated
/// [`FOREIGN_STACK`].
fn on_foreign_stack<T>(f: impl FnOnce() -> T) -> T {
    stacker::grow(FOREIGN_STACK, f)
}

/// Owns a value whose drop recurses deeply (an rnix syntax tree drops one
/// level per nesting level) and drops it on a dedicated stack.
pub struct DeepDrop<T>(ManuallyDrop<T>);

impl<T> DeepDrop<T> {
    pub fn new(value: T) -> Self {
        DeepDrop(ManuallyDrop::new(value))
    }
}

impl<T> Deref for DeepDrop<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for DeepDrop<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T> Drop for DeepDrop<T> {
    fn drop(&mut self) {
        // SAFETY: `self.0` is never used again: this is the end of `self`.
        let value = unsafe { ManuallyDrop::take(&mut self.0) };
        on_foreign_stack(move || drop(value));
    }
}

/// Parses Nix source on a dedicated stack, keeping the result in a
/// [`DeepDrop`].
pub fn parse(text: &str) -> DeepDrop<rnix::Parse<rnix::Root>> {
    DeepDrop::new(on_foreign_stack(|| rnix::Root::parse(text)))
}
