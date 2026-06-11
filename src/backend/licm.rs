//! Loop-invariant code motion — a shared IR→IR pass run on the backend path only
//! (`lower_to_machine_ir`, after `lower`, before `destruct_program`), so both native
//! backends benefit while the interpreter keeps consuming the unoptimized IR as the
//! pristine oracle. The 940-case golden suite validates that hoisting never changes output.
//!
//! **Hoist target.** Rather than synthesize a preheader (which would require rewriting the
//! header's phis), an invariant instruction is moved to the end of the loop header's
//! *immediate dominator* — a block that is always outside the loop and dominates it, so the
//! moved def still dominates all its in-loop uses. This needs no new blocks and no phi
//! surgery (SSA stays valid by construction).
//!
//! **Safety.** Only **pure, non-trapping** instructions are hoisted: arithmetic/bit ops
//! (`Bin` except an *integer* `Div`/`Mod`, which could divide by an invariant zero that a
//! zero-trip loop would never reach), `Un`, `Cast`, `Cmp`, `PtrAdd`, and the address ops.
//! Never `Store`, `Call`/`Prim`/`Mem*` (effects). An instruction is hoisted only when every
//! operand is already defined outside the loop, so a round never reorders interdependent
//! hoists; chains resolve over successive rounds.
//!
//! **Load hoisting (memory-invariant loads).** A `Load` of a *global or stack slot* is hoisted
//! when the location can't change across the loop: its base is a [`MemBase`] (a `GlobalAddr`/
//! `SlotAddr`, not a computed pointer) that is **never address-taken** (so no pointer can alias
//! it), the loop contains **no `Call`/`Prim`** (a callee could write it), and **no `Store`/`Mem*`
//! in the loop writes that exact base**. A global/slot is always a mapped address, so the load
//! never faults — hoisting it (even out of a zero-trip loop) is side-effect-free. This lets a
//! loop counter held in memory (a top-level HolyC scalar) and the address arithmetic that
//! depends on it leave the loop, instead of reloading + recomputing every iteration.

use crate::backend::analysis::{self, Cfg, DomTree, LoopForest};
use crate::ir::*;
use std::collections::{HashMap, HashSet};

const NONE: BlockId = u32::MAX;

/// Run LICM over every function in `p`, returning the optimized program.
pub(crate) fn run(mut p: IrProgram) -> IrProgram {
    for f in &mut p.funcs {
        run_func(f);
    }
    p
}

fn run_func(f: &mut IrFunc) {
    let cfg = Cfg::new(f);
    let dom = DomTree::new(f, &cfg);
    let loops = LoopForest::new(f, &cfg, &dom);

    // Snapshot each loop's (hoist target, body) innermost-first, then drop the analyses so we
    // can mutate `f`. Innermost-first lets an outer pass re-hoist what an inner pass exposed.
    let mut order: Vec<usize> = (0..loops.loops.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(loops.depth[loops.loops[i].header as usize]));
    let mut work: Vec<(BlockId, HashSet<BlockId>)> = Vec::new();
    for &li in &order {
        let l = &loops.loops[li];
        let target = dom.idom[l.header as usize];
        // No valid out-of-loop target (header is the function entry): skip this loop.
        if target == NONE || l.body.contains(&target) {
            continue;
        }
        work.push((target, l.body.clone()));
    }
    drop((loops, dom, cfg));

    // Memory model for load hoisting: which vreg names a global/slot base, and which of those
    // bases ever have their address taken (so a pointer could alias them). Stable under
    // hoisting (it moves instructions, never changes their dsts/operands), so compute it once.
    let mem = MemInfo::new(f);
    for (target, body) in work {
        hoist_loop(f, target, &body, &mem);
    }
}

/// Hoist invariant instructions out of `body` to the end of `target`, to a fixpoint.
fn hoist_loop(f: &mut IrFunc, target: BlockId, body: &HashSet<BlockId>, mem: &MemInfo) {
    let body_blocks: Vec<BlockId> = body.iter().copied().collect();
    // The loop's memory effects, stable across the fixpoint (hoisting only removes loads/pure
    // ops, never adds a `Store`/`Call`): does it call (clobbers everything), and which exact
    // global/slot bases does it write?
    let eff = LoopEffects::new(f, &body_blocks, mem);

    loop {
        let def_block = def_blocks(f);
        // A vreg is loop-invariant iff it is not defined inside the loop body.
        let invariant = |v: Vreg| {
            let db = def_block[v as usize];
            db == NONE || !body.contains(&db)
        };

        let mut hoisted: Vec<IrInst> = Vec::new();
        for &b in &body_blocks {
            let blk = &mut f.blocks[b as usize];
            let mut kept = Vec::with_capacity(blk.insts.len());
            for inst in blk.insts.drain(..) {
                // A pure op (existing) or a memory-invariant load, with every operand (the
                // load's address included) already defined outside the loop.
                let ok = all_uses_invariant(&inst, invariant)
                    && match &inst {
                        IrInst::Load { addr, .. } => load_hoistable(*addr, mem, &eff),
                        other => hoistable(other),
                    };
                if ok {
                    hoisted.push(inst);
                } else {
                    kept.push(inst);
                }
            }
            blk.insts = kept;
        }
        if hoisted.is_empty() {
            break;
        }
        // Append before the target's terminator (its operands all dominate `target`).
        f.blocks[target as usize].insts.extend(hoisted);
    }
}

/// The block defining each vreg: params at the entry, phis at their block, instructions at
/// theirs. `NONE` for a vreg with no definition (an immediate-only / unused index).
fn def_blocks(f: &IrFunc) -> Vec<BlockId> {
    let mut def = vec![NONE; f.n_vregs as usize];
    for p in &f.params {
        def[p.vreg as usize] = f.entry;
    }
    for b in &f.blocks {
        for phi in &b.phis {
            def[phi.dst as usize] = b.id;
        }
        for inst in &b.insts {
            if let Some((d, _)) = analysis::def_of(inst) {
                def[d as usize] = b.id;
            }
        }
    }
    def
}

/// Whether `i` is a pure, non-trapping instruction LICM may relocate.
fn hoistable(i: &IrInst) -> bool {
    match i {
        // Integer divide/modulo can fault on an invariant zero divisor; a zero-trip loop
        // would never reach it, so hoisting could introduce a fault. (Float div/mod yield
        // inf/NaN — no trap — so they are fine.)
        IrInst::Bin { op, ty, .. } => {
            !(matches!(op, IrBinOp::Div | IrBinOp::Mod) && !ty.is_float())
        }
        IrInst::Un { .. }
        | IrInst::Cast { .. }
        | IrInst::Cmp { .. }
        | IrInst::PtrAdd { .. }
        | IrInst::SlotAddr { .. }
        | IrInst::GlobalAddr { .. }
        | IrInst::StrAddr { .. }
        | IrInst::FuncAddr { .. } => true,
        _ => false,
    }
}

/// Whether every vreg `i` reads is loop-invariant (immediates contribute no vreg).
fn all_uses_invariant(i: &IrInst, invariant: impl Fn(Vreg) -> bool) -> bool {
    let mut ok = true;
    analysis::uses_of(i, |r| {
        if !invariant(r) {
            ok = false;
        }
    });
    ok
}

/// A disjoint memory base — a named global or stack slot. Two *different* bases never alias; a
/// computed pointer (anything else) is not a base, so a load through one is never hoisted.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum MemBase {
    Global(GlobalId),
    Slot(SlotId),
}

/// Whole-function memory facts for load hoisting.
struct MemInfo {
    /// Each vreg that is exactly a `GlobalAddr`/`SlotAddr`, mapped to its base (the `off` is
    /// ignored — aliasing is decided per base, conservatively).
    base: HashMap<Vreg, MemBase>,
    /// Bases whose address is *taken* — their `GlobalAddr`/`SlotAddr` flows somewhere other
    /// than a direct memory-access operand, so a pointer could alias them. A base that is never
    /// address-taken can only be written by a *direct* store to it.
    taken: HashSet<MemBase>,
}

impl MemInfo {
    fn new(f: &IrFunc) -> MemInfo {
        let mut base: HashMap<Vreg, MemBase> = HashMap::new();
        for b in &f.blocks {
            for i in &b.insts {
                match i {
                    IrInst::GlobalAddr { dst, global, .. } => {
                        base.insert(*dst, MemBase::Global(*global));
                    }
                    IrInst::SlotAddr { dst, slot, .. } => {
                        base.insert(*dst, MemBase::Slot(*slot));
                    }
                    _ => {}
                }
            }
        }
        // The `addr` of a `Load`/`Store` and a `Mem*` location operand access memory in place —
        // not an escape. Every *other* operand use takes the address (computes with it, passes
        // it, stores it as a value), so it escapes.
        let mut taken: HashSet<MemBase> = HashSet::new();
        for b in &f.blocks {
            for i in &b.insts {
                match i {
                    IrInst::Load { .. } | IrInst::MemZero { .. } | IrInst::MemCpy { .. } => {}
                    IrInst::Store { val, .. } => {
                        if let Val::Reg(v) = val {
                            mark_base(&base, &mut taken, *v);
                        }
                    }
                    other => analysis::uses_of(other, |v| mark_base(&base, &mut taken, v)),
                }
            }
            analysis::term_uses(&b.term, |v| mark_base(&base, &mut taken, v));
        }
        MemInfo { base, taken }
    }

    /// The disjoint base a `Load`/`Store` address denotes, or `None` for a computed pointer.
    fn addr_base(&self, addr: Val) -> Option<MemBase> {
        addr.reg().and_then(|v| self.base.get(&v).copied())
    }
}

fn mark_base(base: &HashMap<Vreg, MemBase>, taken: &mut HashSet<MemBase>, v: Vreg) {
    if let Some(&mb) = base.get(&v) {
        taken.insert(mb);
    }
}

/// A loop's memory writes, used to decide which loads stay invariant across it.
struct LoopEffects {
    /// The loop contains a call — a callee may write any global, so no load is invariant.
    has_call: bool,
    /// The exact bases the loop writes via a *direct* `Store`/`Mem*` to a `GlobalAddr`/`SlotAddr`.
    written: HashSet<MemBase>,
}

impl LoopEffects {
    fn new(f: &IrFunc, body: &[BlockId], mem: &MemInfo) -> LoopEffects {
        let mut has_call = false;
        let mut written: HashSet<MemBase> = HashSet::new();
        for &b in body {
            for i in &f.blocks[b as usize].insts {
                match i {
                    IrInst::Call { .. } | IrInst::Prim { .. } => has_call = true,
                    IrInst::Store { addr, .. } => {
                        if let Some(mb) = mem.addr_base(*addr) {
                            written.insert(mb);
                        }
                    }
                    IrInst::MemZero { dst, .. } | IrInst::MemCpy { dst, .. } => {
                        if let Some(mb) = mem.addr_base(*dst) {
                            written.insert(mb);
                        }
                    }
                    _ => {}
                }
            }
        }
        LoopEffects { has_call, written }
    }
}

/// Whether a `Load` from `addr` is invariant across the loop: a disjoint, never-address-taken
/// base (so only a direct store could write it), no call in the loop, and no store in the loop
/// to that exact base. A global/slot is always a mapped address, so the load can't fault — it is
/// safe to evaluate once in the preheader even if the loop runs zero times.
fn load_hoistable(addr: Val, mem: &MemInfo, eff: &LoopEffects) -> bool {
    if eff.has_call {
        return false;
    }
    match mem.addr_base(addr) {
        Some(mb) => !mem.taken.contains(&mb) && !eff.written.contains(&mb),
        None => false,
    }
}
