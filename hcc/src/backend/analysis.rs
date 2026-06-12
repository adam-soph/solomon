//! Pure IR analyses shared by the register allocator ([`crate::backend::regalloc`]) and the loop
//! optimizer ([`crate::backend::licm`]): the canonical operand/def walkers, and (added in later
//! phases) the CFG, dominator tree, and natural-loop forest.
//!
//! The operand walkers mirror [`crate::ir::Inst::for_each_use`] / [`Term::for_each_use`]
//! exactly, so every consumer here sees the same vregs the verifier checks — a new operand
//! can't be added to one walker and forgotten in another.

// The CFG / dominator / loop types below are consumed by `crate::backend::licm` (Phase 2) and
// `crate::backend::regalloc` (Phase 3); this allow is removed once both are wired.
#![allow(dead_code)]

use crate::ir::*;
use std::collections::{HashMap, HashSet};

/// The control-flow graph of one function: per-block successor / predecessor lists and a
/// reverse-postorder numbering from the entry. Relies on the backend-wide invariant that a
/// block's id equals its index in [`Func::blocks`]. Works on either SSA or phi-free IR —
/// it only reads terminators, so LICM (pre-out-of-SSA) and the allocator (post) share it.
pub(crate) struct Cfg {
    pub succs: Vec<Vec<BlockId>>,
    pub preds: Vec<Vec<BlockId>>,
    /// Blocks in reverse postorder from the entry (unreachable blocks are absent).
    pub rpo: Vec<BlockId>,
    /// Each block's index in `rpo`, or `u32::MAX` if it is unreachable.
    pub rpo_index: Vec<u32>,
}

impl Cfg {
    pub fn new(f: &Func) -> Cfg {
        let nb = f.blocks.len();
        let mut succs = vec![Vec::new(); nb];
        for (i, b) in f.blocks.iter().enumerate() {
            debug_assert_eq!(b.id as usize, i, "block id must equal its index");
            let mut s = b.term.successors();
            s.sort_unstable();
            s.dedup();
            succs[i] = s;
        }
        let mut preds = vec![Vec::new(); nb];
        for (i, ss) in succs.iter().enumerate() {
            for &s in ss {
                preds[s as usize].push(i as BlockId);
            }
        }
        // Iterative postorder DFS from the entry, then reverse.
        let mut rpo = Vec::with_capacity(nb);
        let mut visited = vec![false; nb];
        let mut stack: Vec<(BlockId, usize)> = vec![(f.entry, 0)];
        visited[f.entry as usize] = true;
        while let Some(&(b, idx)) = stack.last() {
            if idx < succs[b as usize].len() {
                stack.last_mut().unwrap().1 += 1;
                let s = succs[b as usize][idx];
                if !visited[s as usize] {
                    visited[s as usize] = true;
                    stack.push((s, 0));
                }
            } else {
                rpo.push(b);
                stack.pop();
            }
        }
        rpo.reverse();
        let mut rpo_index = vec![u32::MAX; nb];
        for (k, &b) in rpo.iter().enumerate() {
            rpo_index[b as usize] = k as u32;
        }
        Cfg {
            succs,
            preds,
            rpo,
            rpo_index,
        }
    }
}

/// The dominator tree, by the Cooper–Harvey–Kennedy iterative algorithm. `idom[b]` is the
/// immediate dominator of `b` (the entry is its own idom); `u32::MAX` marks an unreachable
/// block. Linear-ish in practice and tiny (no external deps).
pub(crate) struct DomTree {
    pub idom: Vec<BlockId>,
}

const NONE: BlockId = u32::MAX;

impl DomTree {
    pub fn new(f: &Func, cfg: &Cfg) -> DomTree {
        let nb = f.blocks.len();
        let mut idom = vec![NONE; nb];
        idom[f.entry as usize] = f.entry;
        let mut changed = true;
        while changed {
            changed = false;
            for &b in &cfg.rpo {
                if b == f.entry {
                    continue;
                }
                let mut new_idom = NONE;
                for &p in &cfg.preds[b as usize] {
                    if idom[p as usize] == NONE {
                        continue; // predecessor not yet processed
                    }
                    new_idom = if new_idom == NONE {
                        p
                    } else {
                        intersect(p, new_idom, &idom, &cfg.rpo_index)
                    };
                }
                if new_idom != NONE && idom[b as usize] != new_idom {
                    idom[b as usize] = new_idom;
                    changed = true;
                }
            }
        }
        DomTree { idom }
    }

    /// Whether `a` dominates `b` (reflexive: `a` dominates itself). False if `b` is
    /// unreachable.
    pub fn dominates(&self, a: BlockId, b: BlockId) -> bool {
        if self.idom[b as usize] == NONE {
            return false;
        }
        let mut x = b;
        loop {
            if x == a {
                return true;
            }
            let i = self.idom[x as usize];
            if i == x {
                return false; // reached the entry without meeting `a`
            }
            x = i;
        }
    }
}

/// Walk two dominator-tree fingers up to their common ancestor, using RPO numbers (a deeper
/// block has a larger RPO index, so it is the one that walks up).
fn intersect(mut a: BlockId, mut b: BlockId, idom: &[BlockId], rpo_index: &[u32]) -> BlockId {
    while a != b {
        while rpo_index[a as usize] > rpo_index[b as usize] {
            a = idom[a as usize];
        }
        while rpo_index[b as usize] > rpo_index[a as usize] {
            b = idom[b as usize];
        }
    }
    a
}

/// A natural loop: a `header` (which dominates the whole loop), its back-edge `latches`, and
/// the `body` (every block that can reach a latch without leaving through the header —
/// header and latches included).
pub(crate) struct NaturalLoop {
    pub header: BlockId,
    pub latches: Vec<BlockId>,
    pub body: HashSet<BlockId>,
}

/// The natural loops of a function plus a per-block nesting depth (how many loop bodies
/// contain each block — the weight the register allocator uses to value hot vregs).
pub(crate) struct LoopForest {
    pub loops: Vec<NaturalLoop>,
    pub depth: Vec<u32>,
}

impl LoopForest {
    pub fn new(f: &Func, cfg: &Cfg, dom: &DomTree) -> LoopForest {
        let nb = f.blocks.len();
        let mut by_header: HashMap<BlockId, NaturalLoop> = HashMap::new();
        for u in 0..nb as BlockId {
            if cfg.rpo_index[u as usize] == NONE {
                continue; // unreachable
            }
            for &v in &cfg.succs[u as usize] {
                if !dom.dominates(v, u) {
                    continue; // not a back-edge
                }
                let loop_ = by_header.entry(v).or_insert_with(|| NaturalLoop {
                    header: v,
                    latches: Vec::new(),
                    body: HashSet::from([v]),
                });
                loop_.latches.push(u);
                // Backward reachability from the latch, never expanding through the header.
                let mut stack = vec![u];
                loop_.body.insert(u);
                while let Some(x) = stack.pop() {
                    if x == v {
                        continue;
                    }
                    for &p in &cfg.preds[x as usize] {
                        if loop_.body.insert(p) {
                            stack.push(p);
                        }
                    }
                }
            }
        }
        let loops: Vec<NaturalLoop> = by_header.into_values().collect();
        let mut depth = vec![0u32; nb];
        for l in &loops {
            for &b in &l.body {
                depth[b as usize] += 1;
            }
        }
        LoopForest { loops, depth }
    }
}

/// The vreg a (non-aggregate) instruction defines, and whether it is a float. The vreg
/// comes from the canonical [`Inst::def`]; the register class is derived from the op's
/// result type (a concern specific to register allocation).
pub(crate) fn def_of(i: &Inst) -> Option<(Vreg, bool)> {
    let dst = i.def()?;
    let is_float = match i {
        Inst::Bin { ty, .. }
        | Inst::Un { ty, .. }
        | Inst::Mov { ty, .. }
        | Inst::Load { ty, .. } => ty.is_float(),
        Inst::Cast { to, .. } => to.is_float(),
        Inst::Call { ret, .. } => matches!(ret, Ret::Scalar(t) if t.is_float()),
        _ => false,
    };
    Some((dst, is_float))
}

/// Visit each vreg an instruction reads, via the canonical [`Inst::for_each_use`] (so
/// analyses see exactly the operands the verifier checks — including an indirect callee).
pub(crate) fn uses_of(i: &Inst, mut f: impl FnMut(Vreg)) {
    i.for_each_use(|val| {
        if let Some(r) = val.reg() {
            f(r);
        }
    });
}

/// Visit each vreg a terminator reads, via the canonical [`Term::for_each_use`].
pub(crate) fn term_uses(t: &Term, mut f: impl FnMut(Vreg)) {
    t.for_each_use(|val| {
        if let Some(r) = val.reg() {
            f(r);
        }
    });
}
