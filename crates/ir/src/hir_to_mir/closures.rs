//! The `convert-closures` pass: makes the regions of `thunk`, `lambda` and
//! `letrec` isolated from above.
//!
//! A value used in such a region but defined outside it becomes a capture: an
//! operand of the op, received by a new argument of each of its regions'
//! blocks (see [`mir::push_capture`]). A value used several regions deep is
//! captured through every region in between.

use std::collections::HashMap;

use pliron::context::{Context, Ptr};
use pliron::linked_list::ContainsLinkedList;
use pliron::operation::Operation;
use pliron::pass::{AnalysisManager, Pass, PassResult};
use pliron::result::Result;
use pliron::value::Value;

use crate::mir;

pub struct ConvertClosures;

impl Pass for ConvertClosures {
    fn name(&self) -> &str {
        "convert-closures"
    }

    fn run(
        &mut self,
        op: Ptr<Operation>,
        ctx: &mut Context,
        _analyses: &mut AnalysisManager,
    ) -> Result<PassResult> {
        let mut closures = Closures {
            ctx,
            frames: Vec::new(),
            depths: HashMap::new(),
            captures: HashMap::new(),
        };
        closures.op(op);
        let mut result = PassResult::default();
        result.ir_changed = (!closures.captures.is_empty()).into();
        Ok(result)
    }
}

struct Closures<'a> {
    ctx: &'a Context,
    /// The isolated regions around the op being visited, innermost last, as
    /// (op, region index).
    frames: Vec<(Ptr<Operation>, usize)>,
    /// The number of frames around the definition of each value visited.
    depths: HashMap<Value, usize>,
    /// The block arguments created for each (isolated op, captured value),
    /// one per region.
    captures: HashMap<(Ptr<Operation>, Value), Vec<Value>>,
}

fn is_isolated(ctx: &Context, op: Ptr<Operation>) -> bool {
    Operation::get_op::<mir::ThunkOp>(op, ctx).is_some()
        || Operation::get_op::<mir::LambdaOp>(op, ctx).is_some()
        || Operation::get_op::<mir::LetrecOp>(op, ctx).is_some()
}

impl Closures<'_> {
    fn op(&mut self, op: Ptr<Operation>) {
        crate::stack::grow(|| self.op_here(op))
    }

    fn op_here(&mut self, op: Ptr<Operation>) {
        let ctx = self.ctx;
        let operands: Vec<Value> = op.deref(ctx).operands().collect();
        for (i, operand) in operands.into_iter().enumerate() {
            let depth = self.depths[&operand];
            if depth < self.frames.len() {
                let captured = self.capture(operand, depth);
                Operation::replace_operand(op, ctx, i, captured);
            }
        }
        let isolated = is_isolated(ctx, op);
        let regions: Vec<_> = op.deref(ctx).regions().collect();
        for (i, region) in regions.into_iter().enumerate() {
            if isolated {
                self.frames.push((op, i));
            }
            let blocks: Vec<_> = region.deref(ctx).iter(ctx).collect();
            for block in blocks {
                for arg in block.deref(ctx).arguments() {
                    self.depths.insert(arg, self.frames.len());
                }
                let nested: Vec<_> = block.deref(ctx).iter(ctx).collect();
                for nested in nested {
                    self.op(nested);
                }
            }
            if isolated {
                self.frames.pop();
            }
        }
        for result in op.deref(ctx).results() {
            self.depths.insert(result, self.frames.len());
        }
    }

    /// `value`, defined inside `depth` frames, as a value of the innermost
    /// frame: captured by each frame from `depth` on.
    fn capture(&mut self, value: Value, depth: usize) -> Value {
        let ctx = self.ctx;
        let mut value = value;
        for &(op, region) in &self.frames[depth..] {
            let args = self
                .captures
                .entry((op, value))
                .or_insert_with(|| mir::push_capture(ctx, op, value));
            value = args[region];
        }
        value
    }
}
