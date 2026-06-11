//! Register allocation: assign each SSA vreg (already out of SSA) a [`Location`] — a
//! physical register or its frame slot — that the per-arch `isel` consults when emitting
//! operand loads/stores. Architecture-neutral IR analysis shared by both backends; the
//! interpreter skips it (it runs the SSA form directly).
//!
//! This is currently the liveness-based linear scan that promotes hot vregs into the
//! target's **callee-saved** registers (everything else stays in its slot). Phase 4 widens
//! it to the full register file with live-range splitting; the [`Allocation`] interface is
//! shaped for that already.

use crate::backend::PReg;
use crate::backend::analysis::{Cfg, DomTree, LoopForest, def_of, term_uses, uses_of};
use crate::ir::*;

/// Where a vreg lives during the function body.
#[derive(Clone, Copy)]
pub enum Location {
    /// In a physical register.
    Reg(PReg),
    /// In its frame slot (the `isel` assigns the offset; operands load/store there).
    Spill,
}

/// The result of allocating one function: a [`Location`] per vreg, plus the **callee-saved**
/// registers the function must preserve (save in the prologue, restore in the epilogue —
/// caller-saved promotions need no save).
pub struct Allocation {
    pub loc: Vec<Location>,
    pub saved: Vec<PReg>,
}

impl Allocation {
    /// Every vreg spilled — the conservative baseline (no register touched, nothing to save).
    fn all_spilled(n: usize) -> Allocation {
        Allocation {
            loc: vec![Location::Spill; n],
            saved: Vec::new(),
        }
    }
}

/// The registers [`allocate`] may use, split by class (int/float) and by who must preserve
/// them. A vreg whose live range **crosses a call** may take only a **callee-saved** register
/// (caller-saved are clobbered by the call); a non-crossing vreg prefers **caller-saved**
/// (free — no prologue save/restore) and falls back to callee-saved. The reserved scratch
/// registers each `isel` needs are simply absent from these pools.
pub struct RegSet {
    pub callee_int: Vec<u32>,
    pub caller_int: Vec<u32>,
    pub callee_flt: Vec<u32>,
    pub caller_flt: Vec<u32>,
}

/// Plan register allocation for `f` (already out of SSA): a liveness-based linear scan over
/// the [`RegSet`] pools. A vreg referenced ≥2 times gets a register when one of its eligible
/// pools (per its class and whether its range crosses a call) has a free one; otherwise it
/// stays spilled. Caller-saved registers are used only for ranges that do not cross a call,
/// so nothing live across a `Call`/`Prim` sits in a register the call would clobber.
///
/// When `program_has_exceptions` is set, **nothing** is allocated, in *any* function — a
/// `throw`'s longjmp unwinds straight to the catching frame, restoring sp/fp but **not** the
/// callee-saved registers; every abandoned intermediate frame skips its epilogue and so
/// never restores its caller's promoted registers. Spilling everything program-wide is the
/// conservative correct baseline. The caller computes the flag once via
/// [`crate::backend::program_has_exceptions`].
pub fn allocate(f: &IrFunc, regs: &RegSet, program_has_exceptions: bool) -> Allocation {
    let n = f.n_vregs as usize;
    if n == 0 || program_has_exceptions {
        return Allocation::all_spilled(n);
    }

    // Float/int class per vreg (a wrong class would put a float in an x-register).
    let mut is_float = vec![false; n];
    for p in &f.params {
        if matches!(p.ty, ArgTy::Float) {
            is_float[p.vreg as usize] = true;
        }
    }
    for b in &f.blocks {
        for i in &b.insts {
            if let Some((d, fl)) = def_of(i) {
                is_float[d as usize] = fl;
            }
        }
    }

    let nb = f.blocks.len();
    // Linear instruction positions: each block occupies `insts.len()` slots then one for
    // its terminator.
    let mut blk_start = vec![0u32; nb];
    let mut blk_term = vec![0u32; nb];
    let mut pos = 0u32;
    for (bi, b) in f.blocks.iter().enumerate() {
        blk_start[bi] = pos;
        pos += b.insts.len() as u32;
        blk_term[bi] = pos;
        pos += 1;
    }

    // Per-block use/def for the liveness dataflow.
    let mut use_set: Vec<std::collections::HashSet<Vreg>> = vec![Default::default(); nb];
    let mut def_set: Vec<std::collections::HashSet<Vreg>> = vec![Default::default(); nb];
    for (bi, b) in f.blocks.iter().enumerate() {
        let mut defined: std::collections::HashSet<Vreg> = Default::default();
        for i in &b.insts {
            uses_of(i, |r| {
                if !defined.contains(&r) {
                    use_set[bi].insert(r);
                }
            });
            if let Some((d, _)) = def_of(i) {
                defined.insert(d);
                def_set[bi].insert(d);
            }
        }
        term_uses(&b.term, |r| {
            if !defined.contains(&r) {
                use_set[bi].insert(r);
            }
        });
    }

    // Backward liveness to a fixpoint.
    let succs: Vec<Vec<usize>> = f
        .blocks
        .iter()
        .map(|b| b.term.successors().iter().map(|&s| s as usize).collect())
        .collect();
    let mut live_in: Vec<std::collections::HashSet<Vreg>> = vec![Default::default(); nb];
    let mut live_out: Vec<std::collections::HashSet<Vreg>> = vec![Default::default(); nb];
    loop {
        let mut changed = false;
        for bi in (0..nb).rev() {
            let mut out: std::collections::HashSet<Vreg> = Default::default();
            for &s in &succs[bi] {
                out.extend(live_in[s].iter().copied());
            }
            let mut in_ = use_set[bi].clone();
            for &r in &out {
                if !def_set[bi].contains(&r) {
                    in_.insert(r);
                }
            }
            if out != live_out[bi] || in_ != live_in[bi] {
                live_out[bi] = out;
                live_in[bi] = in_;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Single live interval [start, end] + a reference count per vreg.
    let mut start = vec![u32::MAX; n];
    let mut end = vec![0u32; n];
    let mut refs = vec![0u32; n];
    for p in &f.params {
        start[p.vreg as usize] = 0;
    }
    for (bi, b) in f.blocks.iter().enumerate() {
        for &r in &live_in[bi] {
            let v = r as usize;
            start[v] = start[v].min(blk_start[bi]);
            end[v] = end[v].max(blk_start[bi]);
        }
        for &r in &live_out[bi] {
            let v = r as usize;
            start[v] = start[v].min(blk_term[bi]);
            end[v] = end[v].max(blk_term[bi]);
        }
        let mut p = blk_start[bi];
        for i in &b.insts {
            uses_of(i, |r| {
                let v = r as usize;
                start[v] = start[v].min(p);
                end[v] = end[v].max(p);
                refs[v] += 1;
            });
            if let Some((d, _)) = def_of(i) {
                let v = d as usize;
                start[v] = start[v].min(p);
                end[v] = end[v].max(p);
            }
            p += 1;
        }
        term_uses(&b.term, |r| {
            let v = r as usize;
            start[v] = start[v].min(p);
            end[v] = end[v].max(p);
            refs[v] += 1;
        });
    }

    // Whether each vreg is live across a call (so it may not hold a caller-saved register,
    // which the call would clobber). Computed precisely by a per-block backward walk from the
    // live-out set, marking every vreg live immediately after a `Call`/`Prim` (other than the
    // one that call defines). This must be exact: the coarse `[start, end]` interval is wrong
    // around loop back-edges — a loop-carried value defined in a latch (a high linear
    // position) is live-in to an earlier block, so its `start` can equal a call's position
    // even though the value is live *across* that call.
    let mut crossing = vec![false; n];
    for (bi, b) in f.blocks.iter().enumerate() {
        let mut live = live_out[bi].clone();
        term_uses(&b.term, |r| {
            live.insert(r);
        });
        for inst in b.insts.iter().rev() {
            if matches!(inst, IrInst::Call { .. } | IrInst::Prim { .. }) {
                let def = def_of(inst).map(|(d, _)| d);
                for &v in &live {
                    if Some(v) != def {
                        crossing[v as usize] = true;
                    }
                }
            }
            if let Some((d, _)) = def_of(inst) {
                live.remove(&d);
            }
            uses_of(inst, |r| {
                live.insert(r);
            });
        }
    }

    // Loop-depth weight per vreg: `refs` counts *static* occurrences, so a value used once
    // per iteration of a deep loop looks as cheap as one used once at the top level. Scale
    // refs by 4^(loop depth of the defining block) so a hot, deeply-nested value outranks a
    // cold one for the scarce registers. This drives eviction below; it never affects
    // correctness (any vreg may always stay spilled).
    let cfg = Cfg::new(f);
    let dom = DomTree::new(f, &cfg);
    let loops = LoopForest::new(f, &cfg, &dom);
    let mut def_block = vec![f.entry; n];
    for b in &f.blocks {
        for phi in &b.phis {
            def_block[phi.dst as usize] = b.id;
        }
        for i in &b.insts {
            if let Some((d, _)) = def_of(i) {
                def_block[d as usize] = b.id;
            }
        }
    }
    let weight: Vec<u64> = (0..n)
        .map(|v| {
            let d = loops.depth[def_block[v] as usize].min(15);
            (refs[v] as u64).saturating_mul(1u64 << (2 * d))
        })
        .collect();

    // Candidates: referenced at least twice, with a real interval; processed in start order
    // (the linear scan's expiry needs that), with eviction by weight when registers run out.
    let mut cands: Vec<usize> = (0..n)
        .filter(|&v| start[v] != u32::MAX && refs[v] >= 2)
        .collect();
    cands.sort_by_key(|&v| (start[v], std::cmp::Reverse(refs[v])));

    // Free pools per (class, caller/callee), reversed so `pop` hands out the first-listed.
    let mut callee_int: Vec<u32> = regs.callee_int.iter().rev().copied().collect();
    let mut caller_int: Vec<u32> = regs.caller_int.iter().rev().copied().collect();
    let mut callee_flt: Vec<u32> = regs.callee_flt.iter().rev().copied().collect();
    let mut caller_flt: Vec<u32> = regs.caller_flt.iter().rev().copied().collect();
    // active: (end, reg, is_float, from_caller, weight, vreg)
    let mut active: Vec<(u32, u32, bool, bool, u64, usize)> = Vec::new();

    let mut plan: Vec<Option<PReg>> = vec![None; n];
    let mut from_caller_of = vec![false; n];
    for &v in &cands {
        let (s, e, fl, cr) = (start[v], end[v], is_float[v], crossing[v]);
        // Expire intervals that ended before this one starts, returning their registers.
        let mut i = 0;
        while i < active.len() {
            if active[i].0 < s {
                let (_, reg, afl, from_caller, _, _) = active.remove(i);
                match (afl, from_caller) {
                    (true, true) => caller_flt.push(reg),
                    (true, false) => callee_flt.push(reg),
                    (false, true) => caller_int.push(reg),
                    (false, false) => callee_int.push(reg),
                }
            } else {
                i += 1;
            }
        }
        // A non-crossing vreg prefers a caller-saved register (no prologue save); a crossing
        // one must take a callee-saved register.
        let (caller, callee) = if fl {
            (&mut caller_flt, &mut callee_flt)
        } else {
            (&mut caller_int, &mut callee_int)
        };
        let pick = if cr {
            callee.pop().map(|r| (r, false))
        } else {
            caller
                .pop()
                .map(|r| (r, true))
                .or_else(|| callee.pop().map(|r| (r, false)))
        };
        let pick = pick.or_else(|| {
            // No free register: evict the lowest-weight active holder of a usable register in
            // this class, but only if it is strictly cheaper than `v`. A crossing `v` needs a
            // callee-saved register, so it can only evict a callee-saved (`!from_caller`)
            // holder; a non-crossing `v` can take over either. The evicted vreg falls back to
            // its slot — always safe — and `v` inherits its exact register and save class.
            let victim = active
                .iter()
                .enumerate()
                .filter(|(_, a)| a.2 == fl && !(cr && a.3) && a.4 < weight[v])
                .min_by_key(|(_, a)| a.4)
                .map(|(ai, _)| ai)?;
            let (_, reg, _, from_caller, _, evicted) = active.remove(victim);
            plan[evicted] = None;
            Some((reg, from_caller))
        });
        if let Some((reg, from_caller)) = pick {
            plan[v] = Some(PReg {
                is_float: fl,
                num: reg,
            });
            from_caller_of[v] = from_caller;
            active.push((e, reg, fl, from_caller, weight[v], v));
        }
    }

    // Convert to the Location/Allocation interface. Only callee-saved promotions need a
    // prologue save/restore; caller-saved ones are free (the function owns them).
    let loc: Vec<Location> = plan
        .iter()
        .map(|p| match p {
            Some(r) => Location::Reg(*r),
            None => Location::Spill,
        })
        .collect();
    let mut saved: Vec<PReg> = Vec::new();
    for v in 0..n {
        if let Some(r) = plan[v] {
            if !from_caller_of[v]
                && !saved
                    .iter()
                    .any(|s| s.num == r.num && s.is_float == r.is_float)
            {
                saved.push(r);
            }
        }
    }
    Allocation { loc, saved }
}
