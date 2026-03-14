// fidan-passes/src/unreachable_pruning.rs
//
// Remove basic blocks never reachable from the function entry block.
// Rather than renumbering all BlockIds (which would require rewriting every
// reference), unreachable blocks are simply cleared: their instruction list is
// emptied and their terminator is set to `Unreachable`.

use fidan_mir::{BlockId, Instr, MirFunction, MirProgram, Terminator};
use std::collections::HashSet;

pub struct UnreachablePruning;

impl crate::Pass for UnreachablePruning {
    fn run(&self, prog: &mut MirProgram) {
        for func in &mut prog.functions {
            if func.blocks.is_empty() {
                continue;
            }
            let live = reachable(func);
            for bb in &mut func.blocks {
                if !live.contains(&bb.id) {
                    bb.phis.clear();
                    bb.instructions.clear();
                    bb.terminator = Terminator::Unreachable;
                }
            }
        }
    }
}

fn reachable(func: &MirFunction) -> HashSet<BlockId> {
    let mut visited: HashSet<BlockId> = HashSet::new();
    let mut queue: Vec<BlockId> = vec![BlockId(0)];
    while let Some(bb_id) = queue.pop() {
        if !visited.insert(bb_id) {
            continue;
        }
        if bb_id.0 as usize >= func.blocks.len() {
            continue;
        }
        let bb = func.block(bb_id);
        // Follow exception handlers too.
        for instr in &bb.instructions {
            if let Instr::PushCatch(catch_bb) = instr {
                queue.push(*catch_bb);
            }
        }
        match &bb.terminator {
            Terminator::Goto(t) => queue.push(*t),
            Terminator::Branch {
                then_bb, else_bb, ..
            } => {
                queue.push(*then_bb);
                queue.push(*else_bb);
            }
            Terminator::Return(_) | Terminator::Throw { .. } | Terminator::Unreachable => {}
        }
    }
    visited
}
