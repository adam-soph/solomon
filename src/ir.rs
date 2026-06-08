//! A low-level, SSA-form intermediate representation for solomon.
//!
//! Historically solomon had **no IR**: the [interpreter](crate::interp) and both
//! native backends walked the typed AST directly. This module is the shared
//! middle-end that replaces that: lowering ([`crate::lower`]) turns the typed,
//! laid-out AST into this IR, then *both* the interpreter executes it and the
//! backends select instructions from it. Because the three consumers read one IR,
//! they agree by construction.
//!
//! Shape: register-based three-address code in **full SSA form** (typed virtual
//! registers, `phi` at control-flow joins), organised as basic blocks per function.
//! It is "near machine code": memory is explicit ([`IrInst::Load`]/[`IrInst::Store`]
//! with a width), addressing is explicit ([`IrInst::SlotAddr`]/[`IrInst::PtrAdd`]),
//! and every arithmetic op carries its width and signedness. The tricky HolyC rules
//! — narrow-int promote-then-truncate, signedness-directed `>>`/`/`/`%`/relationals,
//! float↔int conversion, store/arg/return coercion — are decided **once** during
//! lowering from `e.ty()` and frozen into the ops, so no consumer re-derives them.
//!
//! Memory model (matching what a real machine / GCC does): a local that is a scalar
//! whose address is never taken becomes an SSA value (a [`Vreg`]); everything
//! addressable — aggregates, address-taken locals, globals, the heap — lives in real
//! memory reached through typed loads and stores. Aggregates therefore never have an
//! [`IrTy`]; they are referenced by their address.

use crate::layout::Layouts;

/// The machine type of a value in flight. Aggregates are absent on purpose: they
/// live in memory and are referenced by a `Ptr`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IrTy {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F64,
    /// A pointer; 8 bytes, integer-class in the ABI.
    Ptr,
}

impl IrTy {
    /// Size in bytes.
    pub fn size(self) -> u32 {
        match self {
            IrTy::I8 | IrTy::U8 => 1,
            IrTy::I16 | IrTy::U16 => 2,
            IrTy::I32 | IrTy::U32 => 4,
            IrTy::I64 | IrTy::U64 | IrTy::F64 | IrTy::Ptr => 8,
        }
    }

    /// Whether this is the floating-point class (vs the integer/pointer class).
    pub fn is_float(self) -> bool {
        matches!(self, IrTy::F64)
    }

    /// Whether an integer type is signed. `F64`/`Ptr` report `false`.
    pub fn is_signed(self) -> bool {
        matches!(self, IrTy::I8 | IrTy::I16 | IrTy::I32 | IrTy::I64)
    }
}

/// A virtual register: a single-assignment typed temporary, numbered per function.
pub type Vreg = u32;
/// A basic-block id, an index into [`IrFunc::blocks`].
pub type BlockId = u32;
/// A frame-slot (`alloca`) id, an index into [`IrFunc::slots`].
pub type SlotId = u32;
/// An interned string-literal id, an index into [`IrProgram::strings`].
pub type StrId = u32;
/// A global id, an index into [`IrProgram::globals`].
pub type GlobalId = u32;

/// An operand: an SSA register or an immediate. Immediates let constant folding in
/// lowering flow straight through to the backends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Val {
    Reg(Vreg),
    ImmInt(i64),
    /// An `F64` immediate, as raw bits (so `PartialEq`/`Eq` behave; NaNs compare by
    /// bit pattern).
    ImmF64(u64),
}

impl Val {
    /// The register this operand reads, if any (for liveness/verification).
    pub fn reg(self) -> Option<Vreg> {
        match self {
            Val::Reg(v) => Some(v),
            _ => None,
        }
    }
}

/// A whole program after lowering.
#[derive(Clone, Debug)]
pub struct IrProgram {
    pub funcs: Vec<IrFunc>,
    pub globals: Vec<IrGlobal>,
    /// Interned, NUL-terminated string-literal bytes. [`IrInst::StrAddr`] targets
    /// these; each literal has one stable address (consistent pointer identity).
    pub strings: Vec<Vec<u8>>,
    /// The one shared `repr(C)` layout table, so the interpreter and backends size
    /// and offset aggregates identically.
    pub layouts: Layouts,
}

/// How a function returns its result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IrRet {
    Void,
    Scalar(IrTy),
    /// Returned by value through a caller-provided sret pointer.
    Agg {
        size: u32,
        align: u32,
    },
}

/// How an argument or parameter is passed, per the internal ABI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ArgTy {
    /// An integer/pointer-class scalar of this width.
    Int(IrTy),
    /// A float-class scalar (`F64`).
    Float,
    /// A by-value aggregate, carried by address; the callee copies it.
    AggAddr { size: u32, align: u32 },
}

/// A function parameter. The ABI delivers its incoming value into `vreg` at entry
/// (an address for `AggAddr`); lowering either uses that vreg as the variable's
/// initial SSA value (SSA-able scalars) or stores/copies it into a slot.
#[derive(Clone, Debug)]
pub struct IrParam {
    pub ty: ArgTy,
    pub vreg: Vreg,
    pub name: Option<String>,
}

/// What a frame slot (`alloca`) is for. Informational; all slots are just sized,
/// aligned byte ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotKind {
    /// An address-taken or aggregate local.
    Local,
    /// A spilled parameter or a parameter's local copy.
    Param,
    /// A compiler temporary (e.g. a ternary/`&&`/`||` join, a switch value).
    Temp,
    /// A variadic argument buffer.
    VarargBuf,
    /// The sret result temporary for an aggregate-returning call.
    Sret,
    /// An on-stack exception frame (for `try`).
    ExcFrame,
}

/// A frame slot: a sized, aligned byte range in the current function's frame.
#[derive(Clone, Debug)]
pub struct SlotInfo {
    pub size: u32,
    pub align: u32,
    pub kind: SlotKind,
    /// The source name, kept for diagnostics and the register promoter.
    pub name: Option<String>,
}

/// One function in SSA form.
#[derive(Clone, Debug)]
pub struct IrFunc {
    pub name: String,
    pub ret: IrRet,
    pub params: Vec<IrParam>,
    pub varargs: bool,
    pub slots: Vec<SlotInfo>,
    pub blocks: Vec<IrBlock>,
    pub entry: BlockId,
    /// One past the highest [`Vreg`] used, so consumers can size a register file.
    pub n_vregs: u32,
}

/// A basic block: optional `phi` nodes, a straight-line instruction list, and a
/// single terminator.
#[derive(Clone, Debug)]
pub struct IrBlock {
    pub id: BlockId,
    pub phis: Vec<Phi>,
    pub insts: Vec<IrInst>,
    pub term: IrTerm,
}

/// A `phi`: at block entry, `dst` takes the value flowing in from whichever
/// predecessor control came from. There is exactly one `(pred, val)` pair per
/// predecessor block.
#[derive(Clone, Debug)]
pub struct Phi {
    pub dst: Vreg,
    pub ty: IrTy,
    pub args: Vec<(BlockId, Val)>,
}

/// A three-address instruction. Each defines at most one [`Vreg`] (`dst`).
#[derive(Clone, Debug)]
pub enum IrInst {
    /// `dst = lhs <op> rhs`, computed at width `ty` (the promoted width); `signed`
    /// selects arithmetic-vs-logical shift and signed-vs-unsigned divide/mod.
    Bin {
        dst: Vreg,
        op: IrBinOp,
        ty: IrTy,
        signed: bool,
        lhs: Val,
        rhs: Val,
    },
    /// `dst = <op> src` at width `ty`.
    Un {
        dst: Vreg,
        op: IrUnOp,
        ty: IrTy,
        src: Val,
    },
    /// `dst = (src as to)`. Fully determined by `(from, to)`: integer
    /// truncate/sign-extend/zero-extend, float↔int conversion (signed per `from`'s
    /// signedness for int→float and `to`'s for float→int), and bool normalisation.
    Cast {
        dst: Vreg,
        to: IrTy,
        from: IrTy,
        src: Val,
    },
    /// `dst = (lhs <op> rhs) ? 1 : 0`, at width `ty`. The value form of a comparison
    /// (the branch form lives in [`Cond::Cmp`]).
    Cmp {
        dst: Vreg,
        op: CmpOp,
        ty: IrTy,
        signed: bool,
        lhs: Val,
        rhs: Val,
    },
    /// `dst = src` — a register copy. Not produced by lowering (SSA form has none);
    /// the backend's out-of-SSA pass emits these to resolve `phi` nodes, so a `dst`
    /// may be assigned by several `Mov`s across predecessors (no longer single-static).
    Mov { dst: Vreg, ty: IrTy, src: Val },

    /// `dst = &slot + off`.
    SlotAddr { dst: Vreg, slot: SlotId, off: u32 },
    /// `dst = &global + off`.
    GlobalAddr {
        dst: Vreg,
        global: GlobalId,
        off: u32,
    },
    /// `dst = &string[str]` (the literal's stable address).
    StrAddr { dst: Vreg, str: StrId },
    /// `dst = &func` (a self-resolved function address).
    FuncAddr { dst: Vreg, func: String },
    /// `dst = base + index * stride` (array/pointer indexing, gep-like).
    PtrAdd {
        dst: Vreg,
        base: Val,
        index: Val,
        stride: u32,
    },

    /// `dst = *(ty *)addr`, sign/zero-extending the loaded `ty` to 64 bits.
    Load { dst: Vreg, ty: IrTy, addr: Val },
    /// `*(ty *)addr = val`, truncating `val` to `ty`.
    Store { ty: IrTy, addr: Val, val: Val },
    /// `memcpy(dst, src, len)` — a by-value class/array copy.
    MemCpy { dst: Val, src: Val, len: u32 },
    /// `memset(dst, 0, len)` — zero a slot before a partial initializer.
    MemZero { dst: Val, len: u32 },

    /// A call to an ordinary function (direct or through a pointer). Aggregate
    /// returns go through `sret`.
    Call {
        dst: Option<Vreg>,
        ret: IrRet,
        callee: Callee,
        args: Vec<ArgVal>,
        sret: Option<Val>,
        varargs: VarargInfo,
    },
    /// A primitive intrinsic the backends/interpreter must lower specially (mirrors
    /// [`crate::intrinsics::is_primitive`]). `width` carries the pointee width for
    /// the width-directed atomics.
    Prim {
        dst: Option<Vreg>,
        prim: Prim,
        args: Vec<Val>,
        width: Option<IrTy>,
    },

    /// Open an exception region: push an `ExcFrame` (stored in `frame`) whose landing
    /// pad is block `pad`. A `throw` in this region (or anything it calls) transfers
    /// to `pad`.
    TryBegin { pad: BlockId, frame: SlotId },
    /// Close the most recent exception region on normal completion (pop the frame).
    TryEnd,
}

/// A block terminator.
#[derive(Clone, Debug)]
pub enum IrTerm {
    /// Unconditional branch.
    Br(BlockId),
    /// Branch to `t` if `cond` holds, else `f`.
    CondBr { cond: Cond, t: BlockId, f: BlockId },
    /// A multi-way branch. Each case is an inclusive range `(lo, hi)`; a dense,
    /// all-constant switch is eligible for an O(1) jump table.
    Switch {
        val: Val,
        ty: IrTy,
        signed: bool,
        cases: Vec<(i64, i64, BlockId)>,
        default: BlockId,
    },
    /// Return (a scalar value, or `None` for void / sret-aggregate returns).
    Ret(Option<Val>),
    /// `throw val;` (the value already coerced to `I64`).
    Throw(Val),
    /// A bare `throw;` — re-raise the current `Fs->except_ch`.
    Rethrow,
    /// Control cannot reach here (e.g. after a tail `Exit`).
    Unreachable,
}

/// A branch condition.
#[derive(Clone, Debug)]
pub enum Cond {
    /// Branch on `val != 0` at width `ty`.
    NonZero { val: Val, ty: IrTy },
    /// Branch on a comparison, avoiding a materialised 0/1.
    Cmp {
        op: CmpOp,
        ty: IrTy,
        signed: bool,
        lhs: Val,
        rhs: Val,
    },
}

/// The target of a [`IrInst::Call`].
#[derive(Clone, Debug)]
pub enum Callee {
    Direct(String),
    Indirect(Val),
}

/// One call argument: its ABI class plus the operand carrying it.
#[derive(Clone, Debug)]
pub struct ArgVal {
    pub ty: ArgTy,
    pub val: Val,
}

/// The hidden variadic state placed alongside a call to a `...` function. `buf` is
/// the packed argument buffer (a slot + byte offset) and `count` the number of
/// trailing variadic args.
#[derive(Clone, Debug, Default)]
pub struct VarargInfo {
    pub is_varargs: bool,
    pub buf: Option<(SlotId, u32)>,
    pub count: u32,
}

/// Arithmetic / bitwise / shift binary operators. Comparisons are [`IrInst::Cmp`] /
/// [`Cond::Cmp`]; logical `&&`/`||` are lowered to control flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrBinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

/// Unary operators that produce a value of the same width as their operand. `!x`
/// lowers to a compare-against-zero, `+x` is a no-op, and `*`/`&`/`++`/`--` lower to
/// memory ops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrUnOp {
    Neg,
    BitNot,
}

/// Comparison operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// The primitive intrinsics, one variant per name in
/// [`crate::intrinsics::is_primitive`]. The printf family is deliberately absent: it
/// is pure HolyC, lowered as ordinary [`IrInst::Call`]s that bottom out at
/// [`Prim::StdWrite`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prim {
    StdWrite,
    MAlloc,
    Free,
    HeapExtend,
    MSize,
    UnixNS,
    NanoNS,
    CpuNS,
    Sleep,
    Open,
    LSeek,
    Read,
    Write,
    Close,
    Socket,
    Connect,
    Remove,
    Rename,
    Mkdir,
    Exit,
    Getpid,
    Getppid,
    Getuid,
    Getgid,
    Chdir,
    Getcwd,
    Thread,
    Join,
    AtomicLoad,
    AtomicStore,
    AtomicAdd,
    AtomicSwap,
    AtomicCas,
    AtomicFence,
    FutexWait,
    FutexWake,
}

impl Prim {
    /// Map a primitive intrinsic name (see [`crate::intrinsics::is_primitive`]) to
    /// its [`Prim`], or `None` if it is not a primitive.
    pub fn from_name(name: &str) -> Option<Prim> {
        Some(match name {
            "StdWrite" => Prim::StdWrite,
            "MAlloc" => Prim::MAlloc,
            "Free" => Prim::Free,
            "HeapExtend" => Prim::HeapExtend,
            "MSize" => Prim::MSize,
            "UnixNS" => Prim::UnixNS,
            "NanoNS" => Prim::NanoNS,
            "CpuNS" => Prim::CpuNS,
            "Sleep" => Prim::Sleep,
            "Open" => Prim::Open,
            "LSeek" => Prim::LSeek,
            "Read" => Prim::Read,
            "Write" => Prim::Write,
            "Close" => Prim::Close,
            "Socket" => Prim::Socket,
            "Connect" => Prim::Connect,
            "Remove" => Prim::Remove,
            "Rename" => Prim::Rename,
            "Mkdir" => Prim::Mkdir,
            "Exit" => Prim::Exit,
            "Getpid" => Prim::Getpid,
            "Getppid" => Prim::Getppid,
            "Getuid" => Prim::Getuid,
            "Getgid" => Prim::Getgid,
            "Chdir" => Prim::Chdir,
            "Getcwd" => Prim::Getcwd,
            "Thread" => Prim::Thread,
            "Join" => Prim::Join,
            "AtomicLoad" => Prim::AtomicLoad,
            "AtomicStore" => Prim::AtomicStore,
            "AtomicAdd" => Prim::AtomicAdd,
            "AtomicSwap" => Prim::AtomicSwap,
            "AtomicCas" => Prim::AtomicCas,
            "AtomicFence" => Prim::AtomicFence,
            "FutexWait" => Prim::FutexWait,
            "FutexWake" => Prim::FutexWake,
            _ => return None,
        })
    }
}

// ---- globals ----

/// A global variable: a sized, aligned byte range with a (possibly relocating)
/// initializer. Unmentioned bytes are zero (BSS-style).
#[derive(Clone, Debug)]
pub struct IrGlobal {
    pub name: String,
    pub size: u32,
    pub align: u32,
    pub is_public: bool,
    /// The non-zero initializer leaves, by byte offset. Empty ⇒ zero-initialized.
    pub init: Vec<GlobalInit>,
}

/// One initialized scalar leaf of a global.
#[derive(Clone, Debug)]
pub struct GlobalInit {
    pub off: u32,
    pub ty: IrTy,
    pub val: GlobalConst,
}

/// A compile-time constant usable in a global initializer. The address forms emit a
/// relocation.
#[derive(Clone, Debug)]
pub enum GlobalConst {
    Int(i64),
    F64(u64),
    StrAddr(StrId),
    GlobalAddr(GlobalId, u32),
    FuncAddr(String),
}

// ---- verification ----

impl IrTerm {
    /// The block ids this terminator may transfer to.
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            IrTerm::Br(b) => vec![*b],
            IrTerm::CondBr { t, f, .. } => vec![*t, *f],
            IrTerm::Switch { cases, default, .. } => {
                let mut s: Vec<BlockId> = cases.iter().map(|&(_, _, b)| b).collect();
                s.push(*default);
                s
            }
            IrTerm::Ret(_) | IrTerm::Throw(_) | IrTerm::Rethrow | IrTerm::Unreachable => vec![],
        }
    }
}

/// Structural sanity checks over an [`IrProgram`]: every block/slot/string/global
/// reference is in range, blocks are singly terminated, every [`Vreg`] is defined at
/// most once (SSA) and every used vreg is defined somewhere, and each `phi` has one
/// argument per predecessor block. Returns the list of problems found (empty ⇒ OK).
///
/// This is a cheap guard, not a full dominance verifier; it is intended to run in
/// debug builds and tests. (Dominance — every use dominated by its def — is a
/// follow-up once the dominator tree is computed for register allocation.)
pub fn verify(p: &IrProgram) -> Vec<String> {
    let mut errs = Vec::new();
    for f in &p.funcs {
        verify_func(f, p, &mut errs);
    }
    errs
}

fn verify_func(f: &IrFunc, p: &IrProgram, errs: &mut Vec<String>) {
    let nblocks = f.blocks.len() as u32;
    let here = |what: &str| format!("fn {}: {what}", f.name);

    if f.entry >= nblocks {
        errs.push(here(&format!("entry block {} out of range", f.entry)));
    }
    for (i, b) in f.blocks.iter().enumerate() {
        if b.id != i as u32 {
            errs.push(here(&format!("block {i} has mismatched id {}", b.id)));
        }
    }

    // Predecessor map, derived from terminators.
    let mut preds: Vec<Vec<BlockId>> = vec![Vec::new(); f.blocks.len()];
    for b in &f.blocks {
        for s in b.term.successors() {
            if s < nblocks {
                preds[s as usize].push(b.id);
            } else {
                errs.push(here(&format!(
                    "block {} branches to invalid block {s}",
                    b.id
                )));
            }
        }
    }

    // SSA: collect definitions, check single-assignment.
    let mut defined = vec![false; f.n_vregs as usize];
    let mut def_vreg = |v: Vreg, errs: &mut Vec<String>| {
        if v >= f.n_vregs {
            errs.push(format!("fn {}: vreg %{v} >= n_vregs {}", f.name, f.n_vregs));
        } else if defined[v as usize] {
            errs.push(format!("fn {}: vreg %{v} defined more than once", f.name));
        } else {
            defined[v as usize] = true;
        }
    };
    for prm in &f.params {
        def_vreg(prm.vreg, errs);
    }
    for b in &f.blocks {
        for phi in &b.phis {
            def_vreg(phi.dst, errs);
        }
        for inst in &b.insts {
            if let Some(d) = inst_def(inst) {
                def_vreg(d, errs);
            }
        }
    }

    // Every used vreg must be defined somewhere; check block/slot/global/string refs.
    let use_reg = |v: Vreg, errs: &mut Vec<String>| {
        if v >= f.n_vregs || !defined[v as usize] {
            errs.push(format!("fn {}: use of undefined vreg %{v}", f.name));
        }
    };
    let use_val = |val: Val, errs: &mut Vec<String>| {
        if let Some(v) = val.reg() {
            use_reg(v, errs);
        }
    };
    for b in &f.blocks {
        // phi arity must match the predecessor set.
        let pset = &preds[b.id as usize];
        for phi in &b.phis {
            if phi.args.len() != pset.len() {
                errs.push(here(&format!(
                    "block {}: phi %{} has {} args but {} predecessors",
                    b.id,
                    phi.dst,
                    phi.args.len(),
                    pset.len()
                )));
            }
            for &(pred, val) in &phi.args {
                if !pset.contains(&pred) {
                    errs.push(here(&format!(
                        "block {}: phi %{} names non-predecessor block {pred}",
                        b.id, phi.dst
                    )));
                }
                use_val(val, errs);
            }
        }
        for inst in &b.insts {
            inst_check_refs(inst, f, p, &use_val, errs);
        }
        term_check_refs(&b.term, &use_val, errs);
    }
}

/// The vreg an instruction defines, if any.
fn inst_def(inst: &IrInst) -> Option<Vreg> {
    match *inst {
        IrInst::Bin { dst, .. }
        | IrInst::Un { dst, .. }
        | IrInst::Cast { dst, .. }
        | IrInst::Cmp { dst, .. }
        | IrInst::Mov { dst, .. }
        | IrInst::SlotAddr { dst, .. }
        | IrInst::GlobalAddr { dst, .. }
        | IrInst::StrAddr { dst, .. }
        | IrInst::FuncAddr { dst, .. }
        | IrInst::PtrAdd { dst, .. }
        | IrInst::Load { dst, .. } => Some(dst),
        IrInst::Call { dst, .. } | IrInst::Prim { dst, .. } => dst,
        IrInst::Store { .. }
        | IrInst::MemCpy { .. }
        | IrInst::MemZero { .. }
        | IrInst::TryBegin { .. }
        | IrInst::TryEnd => None,
    }
}

fn inst_check_refs(
    inst: &IrInst,
    f: &IrFunc,
    p: &IrProgram,
    use_val: &dyn Fn(Val, &mut Vec<String>),
    errs: &mut Vec<String>,
) {
    let nslots = f.slots.len() as u32;
    match inst {
        IrInst::Bin { lhs, rhs, .. } | IrInst::Cmp { lhs, rhs, .. } => {
            use_val(*lhs, errs);
            use_val(*rhs, errs);
        }
        IrInst::Un { src, .. } | IrInst::Cast { src, .. } | IrInst::Mov { src, .. } => {
            use_val(*src, errs)
        }
        IrInst::SlotAddr { slot, .. } => {
            if *slot >= nslots {
                errs.push(format!("fn {}: SlotAddr of invalid slot {slot}", f.name));
            }
        }
        IrInst::GlobalAddr { global, .. } => {
            if *global as usize >= p.globals.len() {
                errs.push(format!(
                    "fn {}: GlobalAddr of invalid global {global}",
                    f.name
                ));
            }
        }
        IrInst::StrAddr { str, .. } => {
            if *str as usize >= p.strings.len() {
                errs.push(format!("fn {}: StrAddr of invalid string {str}", f.name));
            }
        }
        IrInst::FuncAddr { .. } => {}
        IrInst::PtrAdd { base, index, .. } => {
            use_val(*base, errs);
            use_val(*index, errs);
        }
        IrInst::Load { addr, .. } => use_val(*addr, errs),
        IrInst::Store { addr, val, .. } => {
            use_val(*addr, errs);
            use_val(*val, errs);
        }
        IrInst::MemCpy { dst, src, .. } => {
            use_val(*dst, errs);
            use_val(*src, errs);
        }
        IrInst::MemZero { dst, .. } => use_val(*dst, errs),
        IrInst::Call {
            callee, args, sret, ..
        } => {
            if let Callee::Indirect(v) = callee {
                use_val(*v, errs);
            }
            for a in args {
                use_val(a.val, errs);
            }
            if let Some(s) = sret {
                use_val(*s, errs);
            }
        }
        IrInst::Prim { args, .. } => {
            for a in args {
                use_val(*a, errs);
            }
        }
        IrInst::TryBegin { pad, frame } => {
            if *pad >= f.blocks.len() as u32 {
                errs.push(format!(
                    "fn {}: TryBegin to invalid pad block {pad}",
                    f.name
                ));
            }
            if *frame >= nslots {
                errs.push(format!(
                    "fn {}: TryBegin with invalid frame slot {frame}",
                    f.name
                ));
            }
        }
        IrInst::TryEnd => {}
    }
}

fn term_check_refs(term: &IrTerm, use_val: &dyn Fn(Val, &mut Vec<String>), errs: &mut Vec<String>) {
    match term {
        IrTerm::CondBr { cond, .. } => match cond {
            Cond::NonZero { val, .. } => use_val(*val, errs),
            Cond::Cmp { lhs, rhs, .. } => {
                use_val(*lhs, errs);
                use_val(*rhs, errs);
            }
        },
        IrTerm::Switch { val, .. } => use_val(*val, errs),
        IrTerm::Ret(Some(v)) | IrTerm::Throw(v) => use_val(*v, errs),
        IrTerm::Br(_) | IrTerm::Ret(None) | IrTerm::Rethrow | IrTerm::Unreachable => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Layouts;

    /// `I64 add(I64 a, I64 b) { return a + b; }` hand-built and verified clean.
    fn add_func() -> IrFunc {
        // params land in %0, %1; %2 = a + b.
        IrFunc {
            name: "add".into(),
            ret: IrRet::Scalar(IrTy::I64),
            params: vec![
                IrParam {
                    ty: ArgTy::Int(IrTy::I64),
                    vreg: 0,
                    name: Some("a".into()),
                },
                IrParam {
                    ty: ArgTy::Int(IrTy::I64),
                    vreg: 1,
                    name: Some("b".into()),
                },
            ],
            varargs: false,
            slots: vec![],
            blocks: vec![IrBlock {
                id: 0,
                phis: vec![],
                insts: vec![IrInst::Bin {
                    dst: 2,
                    op: IrBinOp::Add,
                    ty: IrTy::I64,
                    signed: true,
                    lhs: Val::Reg(0),
                    rhs: Val::Reg(1),
                }],
                term: IrTerm::Ret(Some(Val::Reg(2))),
            }],
            entry: 0,
            n_vregs: 3,
        }
    }

    fn prog(f: IrFunc) -> IrProgram {
        IrProgram {
            funcs: vec![f],
            globals: vec![],
            strings: vec![],
            layouts: Layouts::default(),
        }
    }

    #[test]
    fn verify_accepts_a_wellformed_function() {
        assert_eq!(verify(&prog(add_func())), Vec::<String>::new());
    }

    #[test]
    fn verify_catches_use_of_undefined_vreg() {
        let mut f = add_func();
        // Make the add read an undefined %9.
        if let IrInst::Bin { rhs, .. } = &mut f.blocks[0].insts[0] {
            *rhs = Val::Reg(9);
        }
        let errs = verify(&prog(f));
        assert!(
            errs.iter().any(|e| e.contains("undefined vreg %9")),
            "{errs:?}"
        );
    }

    #[test]
    fn verify_catches_double_definition() {
        let mut f = add_func();
        // Redefine %0 (already a parameter).
        f.blocks[0].insts.push(IrInst::Bin {
            dst: 0,
            op: IrBinOp::Sub,
            ty: IrTy::I64,
            signed: true,
            lhs: Val::Reg(2),
            rhs: Val::ImmInt(1),
        });
        let errs = verify(&prog(f));
        assert!(
            errs.iter().any(|e| e.contains("defined more than once")),
            "{errs:?}"
        );
    }

    #[test]
    fn verify_catches_bad_branch_target() {
        let mut f = add_func();
        f.blocks[0].term = IrTerm::Br(7);
        let errs = verify(&prog(f));
        assert!(
            errs.iter().any(|e| e.contains("invalid block 7")),
            "{errs:?}"
        );
    }
}
