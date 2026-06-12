//! The conformance oracle: a tree-of-blocks interpreter for the SSA [IR](crate::ir).
//!
//! Every native backend matches this interpreter byte-for-byte. It executes an
//! [`Program`] directly; the `run_to_string`/`run_to_bytes` entry points at the end of
//! this module lower a parsed, type-checked program to SSA IR and run it here. It covers
//! the whole implemented language: scalar and aggregate computation,
//! control flow (incl. `switch`), direct/indirect calls and aggregate-by-value (sret), the
//! pure-HolyC printf path (varargs), globals, string literals, exceptions (`try`/`throw`
//! over `Fs`), and the full impure-primitive set (clock, fd/file I/O, sockets, fs mutation,
//! process ids, threads run synchronously at spawn, atomics, futex) plus real
//! argv/env/stdin and `Exit`.
//!
//! Memory is three disjoint regions in one flat address space (the "what GCC does"
//! model: pointers are real addresses): a per-call **stack** (bump-allocated, reclaimed
//! on return), a read-mostly **data** region for string literals and globals, and a
//! **heap** for `MAlloc`. Functions occupy a fourth synthetic range so `&Func` and
//! indirect calls work. Arithmetic, comparison, and casts read the signedness/width that
//! lowering already froze into the IR ops (`Bin`/`Cmp`'s `signed`/`ty`, `Cast{from,to}`),
//! so the interpreter and the backends can't re-derive them differently.
//!
//! The impure primitives are conformance-tested by *property* (e.g. a monotonic clock, a
//! write→read round-trip), not value-pinned against a backend; the deterministic corpus is
//! value-pinned by the differential tests in this module's `tests`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::backend::CodegenError;
use crate::ir::*;

/// Base address of the data region (string literals and globals).
const DATA_BASE: u64 = 0x1000_0000;
/// Base address of the heap region (`MAlloc`).
const HEAP_BASE: u64 = 0x4000_0000;
/// Base of the synthetic function-pointer address space: `&Func` yields
/// `FUNC_BASE + index`, and an indirect call maps the address back to the function.
const FUNC_BASE: u64 = 0x8000_0000;

/// A runtime value in flight: an SSA register's contents. Pointers are addresses
/// (integers into [`Mem`]).
#[derive(Clone, Copy, Debug)]
pub enum RVal {
    Int(i64),
    Float(f64),
}

impl RVal {
    fn as_i64(self) -> i64 {
        match self {
            RVal::Int(i) => i,
            RVal::Float(f) => f as i64,
        }
    }

    fn as_f64(self) -> f64 {
        match self {
            RVal::Int(i) => i as f64,
            RVal::Float(f) => f,
        }
    }

    fn truthy(self) -> bool {
        match self {
            RVal::Int(i) => i != 0,
            RVal::Float(f) => f != 0.0,
        }
    }
}

/// A flat byte-addressable memory in three disjoint regions (stack/data/heap), each
/// a `Vec<u8>` indexed by an address minus its region base. The stack starts past a
/// 16-byte reserved hole so address 0 is null. `out` accumulates `StdWrite` to fd 1.
struct Mem {
    stack: Vec<u8>,
    data: Vec<u8>,
    heap: Vec<u8>,
    heap_sizes: HashMap<u64, u64>,
    out: Vec<u8>,
    /// Open file descriptors for the POSIX fd primitives (a thin emulator over
    /// `std::fs`/`std::net`), plus the next synthetic fd past stdin/out/err.
    fds: HashMap<i64, FdObj>,
    next_fd: i64,
    /// `Thread`/`Join` run synchronously (the function body runs at spawn and its
    /// return is stashed here for `Join`).
    thread_results: HashMap<i64, i64>,
    next_thread: i64,
    /// The program's standard input (fd 0).
    input: Box<dyn std::io::Read>,
    /// Set by `Exit(code)`; unwinds the run and finishes with the output so far.
    exit: Option<i64>,
    /// Set by `ThreadExit(ret)`; unwinds to the nearest `Prim::Thread` spawn, which
    /// takes it as the thread's join value. On the main flow (no spawn to catch it)
    /// the run entries treat it like `Exit`.
    thread_exit: Option<i64>,
    /// The last Win32 error, modeled for `GetLastError` (`Prim::WinCall`): a failed
    /// kernel32 call sets it, a successful one leaves it. Best-effort — the exact
    /// codes need not match a real Windows host, which only the host-independent
    /// interpreter tests assert, never the PE-vs-interp conformance comparison.
    last_error: u32,
    /// In-memory Windows registry for the `Reg*` `Prim::WinCall`s — a genuinely
    /// Windows-only API with no host equivalent, so it is modeled rather than backed by
    /// anything real. `reg_open` maps an open `HKEY` handle to its key path (seeded with
    /// the predefined roots); `reg_values` maps `(key path, value name)` to
    /// `(bytes, type)`. This lets a `<windows.hc>` registry program run and round-trip on
    /// any host; a self-contained create→set→query program matches a real Windows PE.
    reg_open: HashMap<i64, String>,
    reg_values: HashMap<(String, String), (Vec<u8>, u32)>,
    next_hkey: i64,
}

impl Mem {
    fn new(data: Vec<u8>, input: Box<dyn std::io::Read>) -> Self {
        Mem {
            stack: vec![0u8; 16],
            data,
            heap: Vec::new(),
            heap_sizes: HashMap::new(),
            out: Vec::new(),
            fds: HashMap::new(),
            next_fd: 3,
            thread_results: HashMap::new(),
            next_thread: 1,
            input,
            exit: None,
            thread_exit: None,
            last_error: 0,
            reg_open: HashMap::from([
                (-2147483648i64, "HKCR".to_string()), // HKEY_CLASSES_ROOT
                (-2147483647i64, "HKCU".to_string()), // HKEY_CURRENT_USER
                (-2147483646i64, "HKLM".to_string()), // HKEY_LOCAL_MACHINE
            ]),
            reg_values: HashMap::new(),
            next_hkey: 0x10000,
        }
    }

    /// Read a NUL-terminated C string from `addr`.
    fn read_cstr(&self, addr: u64) -> Vec<u8> {
        let (v, off) = self.region(addr);
        let mut out = Vec::new();
        let mut i = off;
        while let Some(&b) = v.get(i) {
            if b == 0 {
                break;
            }
            out.push(b);
            i += 1;
        }
        out
    }

    /// The `Vec` and in-region offset backing `addr`.
    fn region(&self, addr: u64) -> (&[u8], usize) {
        if addr >= HEAP_BASE {
            (&self.heap, (addr - HEAP_BASE) as usize)
        } else if addr >= DATA_BASE {
            (&self.data, (addr - DATA_BASE) as usize)
        } else {
            (&self.stack, addr as usize)
        }
    }

    fn region_mut(&mut self, addr: u64) -> (&mut Vec<u8>, usize) {
        if addr >= HEAP_BASE {
            (&mut self.heap, (addr - HEAP_BASE) as usize)
        } else if addr >= DATA_BASE {
            (&mut self.data, (addr - DATA_BASE) as usize)
        } else {
            (&mut self.stack, addr as usize)
        }
    }

    fn read_bytes(&self, addr: u64, n: usize) -> Vec<u8> {
        let (v, off) = self.region(addr);
        (0..n)
            .map(|k| v.get(off + k).copied().unwrap_or(0))
            .collect()
    }

    fn write_bytes(&mut self, addr: u64, bytes: &[u8]) {
        let (v, off) = self.region_mut(addr);
        if v.len() < off + bytes.len() {
            v.resize(off + bytes.len(), 0);
        }
        v[off..off + bytes.len()].copy_from_slice(bytes);
    }

    fn load(&self, addr: u64, ty: Ty) -> RVal {
        let bytes = self.read_bytes(addr, ty.size() as usize);
        let mut raw = [0u8; 8];
        raw[..bytes.len()].copy_from_slice(&bytes);
        let u = u64::from_le_bytes(raw);
        match ty {
            Ty::F64 => RVal::Float(f64::from_bits(u)),
            Ty::I8 => RVal::Int(u as u8 as i8 as i64),
            Ty::U8 => RVal::Int(u as u8 as i64),
            Ty::I16 => RVal::Int(u as u16 as i16 as i64),
            Ty::U16 => RVal::Int(u as u16 as i64),
            Ty::I32 => RVal::Int(u as u32 as i32 as i64),
            Ty::U32 => RVal::Int(u as u32 as i64),
            Ty::I64 | Ty::U64 | Ty::Ptr => RVal::Int(u as i64),
        }
    }

    fn store(&mut self, addr: u64, ty: Ty, v: RVal) {
        let n = ty.size() as usize;
        let bits: u64 = match ty {
            Ty::F64 => v.as_f64().to_bits(),
            _ => v.as_i64() as u64,
        };
        self.write_bytes(addr, &bits.to_le_bytes()[..n]);
    }

    fn memcpy(&mut self, dst: u64, src: u64, len: u32) {
        let tmp = self.read_bytes(src, len as usize);
        self.write_bytes(dst, &tmp);
    }

    fn memzero(&mut self, dst: u64, len: u32) {
        self.write_bytes(dst, &vec![0u8; len as usize]);
    }

    fn alloc_stack(&mut self, size: u32, align: u32) -> u64 {
        let align = align.max(1) as usize;
        let aligned = self.stack.len().div_ceil(align) * align;
        let end = aligned + size.max(1) as usize;
        if self.stack.len() < end {
            self.stack.resize(end, 0);
        }
        aligned as u64
    }

    fn alloc_heap(&mut self, size: i64) -> u64 {
        let size = size.max(0) as usize;
        let aligned = self.heap.len().div_ceil(16) * 16;
        self.heap.resize(aligned + size.max(1), 0);
        let addr = HEAP_BASE + aligned as u64;
        self.heap_sizes.insert(addr, size as u64);
        addr
    }
}

/// How a function invocation finished: a normal return, or an uncaught exception
/// that is unwinding to a caller's `try` (its value lives in `Fs->except_ch`).
enum Outcome {
    Returned(Option<RVal>),
    Threw,
}

/// Whether a straight-line instruction completed normally or propagated a throw (a
/// call whose callee threw without catching it).
enum InstFlow {
    Normal,
    Threw,
}

/// The conformance oracle: an IR interpreter over one program. Holds the static data
/// image (string literals and globals) laid out once; each run seeds a fresh [`Mem`]
/// from it.
pub struct Oracle<'p> {
    prog: &'p Program,
    funcs: HashMap<&'p str, &'p Func>,
    /// Functions by synthetic address index, for indirect calls (`FUNC_BASE + i`).
    func_list: Vec<&'p Func>,
    func_index: HashMap<&'p str, usize>,
    data: Vec<u8>,
    str_addr: Vec<u64>,
    global_addr: Vec<u64>,
    /// Command-line arguments exposed via `argc`/`argv` (`args[0]` is the program name,
    /// so the count is always ≥ 1).
    args: Vec<String>,
    /// The program's standard input (fd 0), consumed on the next `run`/`run_program`.
    input: std::cell::RefCell<Option<Box<dyn std::io::Read>>>,
}

impl<'p> Oracle<'p> {
    pub fn new(prog: &'p Program) -> Self {
        // Lay out the static data region: string literals first, then globals. Each
        // gets a stable address; globals are zero-filled (a runtime initializer runs
        // in `@entry`).
        let mut data = Vec::new();
        let mut str_addr = Vec::with_capacity(prog.strings.len());
        for s in &prog.strings {
            str_addr.push(DATA_BASE + data.len() as u64);
            data.extend_from_slice(s);
        }
        let mut global_addr = Vec::with_capacity(prog.globals.len());
        for g in &prog.globals {
            let align = g.align.max(1) as usize;
            while data.len() % align != 0 {
                data.push(0);
            }
            global_addr.push(DATA_BASE + data.len() as u64);
            data.resize(data.len() + g.size.max(1) as usize, 0);
        }
        let funcs = prog.funcs.iter().map(|f| (f.name.as_str(), f)).collect();
        let func_list: Vec<&Func> = prog.funcs.iter().collect();
        let func_index = func_list
            .iter()
            .enumerate()
            .map(|(i, f)| (f.name.as_str(), i))
            .collect();
        Oracle {
            prog,
            funcs,
            func_list,
            func_index,
            data,
            str_addr,
            global_addr,
            args: vec!["hcc".to_string()],
            input: std::cell::RefCell::new(None),
        }
    }

    /// Set the command line visible through `argc`/`argv` (`args[0]` = program name).
    pub fn set_args(&mut self, args: Vec<String>) {
        if !args.is_empty() {
            self.args = args;
        }
    }

    /// Set the program's standard input (fd 0), consumed by the next run.
    pub fn set_input(&mut self, input: Box<dyn std::io::Read>) {
        *self.input.borrow_mut() = Some(input);
    }

    /// Resolve a function-pointer address to its function.
    fn func_at(&self, addr: u64) -> Option<&'p Func> {
        if addr < FUNC_BASE {
            return None;
        }
        self.func_list.get((addr - FUNC_BASE) as usize).copied()
    }

    /// A fresh memory image with `Fs` seeded to a zeroed `CTask` region and the command
    /// line / environment seeded.
    fn fresh_mem(&self) -> Mem {
        let input = self
            .input
            .borrow_mut()
            .take()
            .unwrap_or_else(|| Box::new(std::io::empty()));
        let mut mem = Mem::new(self.data.clone(), input);
        if let Some(idx) = self.prog.globals.iter().position(|g| g.name == "Fs") {
            let size = self
                .prog
                .layouts
                .size_of(&crate::ast::Type::Named("CTask".to_string()))
                .max(8);
            let task = mem.alloc_heap(size as i64);
            mem.store(self.global_addr[idx], Ty::Ptr, RVal::Int(task as i64));
        }
        self.seed_command_line(&mut mem);
        mem
    }

    /// Seed `argc`/`argv`/`envp` (when the program registered them) to the configured
    /// command line (`args[0]` is the program name) and the real process environment —
    /// matching the native backends.
    fn seed_command_line(&self, mem: &mut Mem) {
        let store_global = |mem: &mut Mem, name: &str, v: i64| {
            if let Some(idx) = self.prog.globals.iter().position(|g| g.name == name) {
                mem.store(self.global_addr[idx], Ty::Ptr, RVal::Int(v));
                true
            } else {
                false
            }
        };
        // Build a NULL-terminated `char *[]` of NUL-terminated strings; return its base.
        let make_argv = |mem: &mut Mem, items: &[Vec<u8>]| -> u64 {
            let base = mem.alloc_heap(((items.len() + 1) * 8) as i64);
            for (i, s) in items.iter().enumerate() {
                let p = mem.alloc_heap((s.len() + 1) as i64);
                mem.write_bytes(p, s);
                mem.write_bytes(p + s.len() as u64, &[0]);
                mem.store(base + (i * 8) as u64, Ty::Ptr, RVal::Int(p as i64));
            }
            mem.store(base + (items.len() * 8) as u64, Ty::Ptr, RVal::Int(0));
            base
        };
        if store_global(mem, "argc", self.args.len() as i64) {
            let argv: Vec<Vec<u8>> = self.args.iter().map(|a| a.as_bytes().to_vec()).collect();
            let base = make_argv(mem, &argv);
            store_global(mem, "argv", base as i64);
        }
        if self.prog.globals.iter().any(|g| g.name == "envp") {
            let env: Vec<Vec<u8>> = std::env::vars_os()
                .map(|(k, v)| {
                    let mut s = k.into_encoded_bytes();
                    s.push(b'=');
                    s.extend_from_slice(&v.into_encoded_bytes());
                    s
                })
                .collect();
            let base = make_argv(mem, &env);
            store_global(mem, "envp", base as i64);
        }
    }

    /// Run function `name` with `args`, returning its result (`None` for void or an
    /// uncaught throw).
    pub fn run(&self, name: &str, args: &[RVal]) -> Result<Option<RVal>, CodegenError> {
        let f = self
            .funcs
            .get(name)
            .ok_or_else(|| CodegenError::new(format!("no such function: {name}"), None))?;
        let mut mem = self.fresh_mem();
        match self.exec_func(f, args, &mut mem) {
            Ok(Outcome::Returned(v)) => Ok(v),
            Ok(Outcome::Threw) => Ok(None),
            // `Exit` — or a main-flow `ThreadExit` (thrd_exit outside any thread
            // terminates the program) — unwinds cleanly.
            Err(_) if mem.exit.is_some() || mem.thread_exit.is_some() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Run the synthesised top-level entry, returning the **raw** `StdWrite`-to-fd-1 output
    /// bytes (the program's real stdout — not lossy-decoded, so a non-UTF-8 byte a program
    /// emits is preserved for a byte-exact comparison against a native binary). An uncaught
    /// throw — or `Exit` — finishes cleanly after the output so far, matching the oracle.
    pub fn run_program(&self) -> Result<Vec<u8>, CodegenError> {
        let mut mem = self.fresh_mem();
        if let Some(f) = self.funcs.get(crate::lower::ENTRY) {
            match self.exec_func(f, &[], &mut mem) {
                Ok(_) => {}
                // `Exit` (or a main-flow `ThreadExit`): finish with the output so far.
                Err(_) if mem.exit.is_some() || mem.thread_exit.is_some() => {}
                Err(e) => return Err(e),
            }
        }
        Ok(mem.out)
    }

    fn exec_func(&self, f: &Func, args: &[RVal], mem: &mut Mem) -> Result<Outcome, CodegenError> {
        let frame_start = mem.stack.len();
        let slot_base: Vec<u64> = f
            .slots
            .iter()
            .map(|s| mem.alloc_stack(s.size, s.align))
            .collect();

        let mut regs = vec![RVal::Int(0); f.n_vregs as usize];
        for (i, p) in f.params.iter().enumerate() {
            if let Some(a) = args.get(i) {
                regs[p.vreg as usize] = *a;
            }
        }

        let result = self.run_blocks(f, &mut regs, &slot_base, mem);
        mem.stack.truncate(frame_start);
        result
    }

    fn run_blocks(
        &self,
        f: &Func,
        regs: &mut [RVal],
        slot_base: &[u64],
        mem: &mut Mem,
    ) -> Result<Outcome, CodegenError> {
        // Active `try` landing pads, innermost last (a per-frame exception stack).
        let mut try_stack: Vec<BlockId> = Vec::new();
        let mut cur = f.entry;
        let mut prev: Option<BlockId> = None;
        'blocks: loop {
            let b = &f.blocks[cur as usize];

            if let Some(p) = prev {
                let resolved: Vec<(Vreg, RVal)> = b
                    .phis
                    .iter()
                    .map(|phi| (phi.dst, self.eval_phi(phi, p, regs)))
                    .collect();
                for (d, v) in resolved {
                    regs[d as usize] = v;
                }
            }

            for inst in &b.insts {
                match inst {
                    Inst::TryBegin { pad, .. } => try_stack.push(*pad),
                    Inst::TryEnd => {
                        try_stack.pop();
                    }
                    _ => match self.exec_inst(inst, regs, slot_base, mem)? {
                        InstFlow::Normal => {}
                        InstFlow::Threw => match try_stack.pop() {
                            Some(pad) => {
                                prev = Some(cur);
                                cur = pad;
                                continue 'blocks;
                            }
                            None => return Ok(Outcome::Threw),
                        },
                    },
                }
            }

            match &b.term {
                Term::Br(t) => {
                    prev = Some(cur);
                    cur = *t;
                }
                Term::CondBr { cond, t, f: fb } => {
                    let take = self.eval_cond(cond, regs);
                    prev = Some(cur);
                    cur = if take { *t } else { *fb };
                }
                Term::Switch {
                    val,
                    cases,
                    default,
                    ..
                } => {
                    let v = self.rd(*val, regs).as_i64();
                    let target = cases
                        .iter()
                        .find(|&&(lo, hi, _)| lo <= v && v <= hi)
                        .map(|&(_, _, b)| b)
                        .unwrap_or(*default);
                    prev = Some(cur);
                    cur = target;
                }
                Term::Ret(v) => return Ok(Outcome::Returned(v.map(|v| self.rd(v, regs)))),
                // The throw value and `Fs` flags were written by the preceding stores;
                // here we only transfer control to the nearest handler, else unwind.
                Term::Throw(_) | Term::Rethrow => match try_stack.pop() {
                    Some(pad) => {
                        prev = Some(cur);
                        cur = pad;
                    }
                    None => return Ok(Outcome::Threw),
                },
                Term::Unreachable => {
                    return Err(CodegenError::new("reached an unreachable block", None));
                }
            }
        }
    }

    fn eval_phi(&self, phi: &Phi, prev: BlockId, regs: &[RVal]) -> RVal {
        for &(b, v) in &phi.args {
            if b == prev {
                return self.rd(v, regs);
            }
        }
        RVal::Int(0)
    }

    fn rd(&self, v: Val, regs: &[RVal]) -> RVal {
        match v {
            Val::Reg(r) => regs[r as usize],
            Val::ImmInt(i) => RVal::Int(i),
            Val::ImmF64(b) => RVal::Float(f64::from_bits(b)),
        }
    }

    fn eval_cond(&self, cond: &Cond, regs: &[RVal]) -> bool {
        match cond {
            Cond::NonZero { val, .. } => self.rd(*val, regs).truthy(),
            Cond::Cmp {
                op,
                ty,
                signed,
                lhs,
                rhs,
            } => {
                let l = self.rd(*lhs, regs);
                let r = self.rd(*rhs, regs);
                cmp(*op, *ty, *signed, l, r)
            }
        }
    }

    fn exec_inst(
        &self,
        inst: &Inst,
        regs: &mut [RVal],
        slot_base: &[u64],
        mem: &mut Mem,
    ) -> Result<InstFlow, CodegenError> {
        match inst {
            Inst::Bin {
                dst,
                op,
                ty,
                signed,
                lhs,
                rhs,
            } => {
                let l = self.rd(*lhs, regs);
                let r = self.rd(*rhs, regs);
                regs[*dst as usize] = bin(*op, *ty, *signed, l, r)?;
            }
            Inst::Un { dst, op, ty, src } => {
                let v = self.rd(*src, regs);
                regs[*dst as usize] = match op {
                    UnOp::Neg => {
                        if ty.is_float() {
                            RVal::Float(-v.as_f64())
                        } else {
                            RVal::Int(v.as_i64().wrapping_neg())
                        }
                    }
                    UnOp::BitNot => RVal::Int(!v.as_i64()),
                    UnOp::Popcount => RVal::Int(v.as_i64().count_ones() as i64),
                };
            }
            Inst::Cast { dst, to, src, .. } => {
                let v = self.rd(*src, regs);
                regs[*dst as usize] = cast(*to, v);
            }
            Inst::Mov { dst, src, .. } => {
                regs[*dst as usize] = self.rd(*src, regs);
            }
            Inst::Cmp {
                dst,
                op,
                ty,
                signed,
                lhs,
                rhs,
            } => {
                let l = self.rd(*lhs, regs);
                let r = self.rd(*rhs, regs);
                regs[*dst as usize] = RVal::Int(i64::from(cmp(*op, *ty, *signed, l, r)));
            }
            Inst::SlotAddr { dst, slot, off } => {
                regs[*dst as usize] = RVal::Int(slot_base[*slot as usize] as i64 + *off as i64);
            }
            Inst::StrAddr { dst, str } => {
                regs[*dst as usize] = RVal::Int(self.str_addr[*str as usize] as i64);
            }
            Inst::GlobalAddr { dst, global, off } => {
                regs[*dst as usize] =
                    RVal::Int(self.global_addr[*global as usize] as i64 + *off as i64);
            }
            Inst::FuncAddr { dst, func } => {
                let idx = self.func_index.get(func.as_str()).ok_or_else(|| {
                    CodegenError::new(format!("address of unknown function: {func}"), None)
                })?;
                regs[*dst as usize] = RVal::Int((FUNC_BASE + *idx as u64) as i64);
            }
            Inst::PtrAdd {
                dst,
                base,
                index,
                stride,
            } => {
                let b = self.rd(*base, regs).as_i64();
                let i = self.rd(*index, regs).as_i64();
                regs[*dst as usize] = RVal::Int(b + i * (*stride as i64));
            }
            Inst::Load { dst, ty, addr } => {
                let a = self.rd(*addr, regs).as_i64() as u64;
                regs[*dst as usize] = mem.load(a, *ty);
            }
            Inst::Store { ty, addr, val } => {
                let a = self.rd(*addr, regs).as_i64() as u64;
                let v = self.rd(*val, regs);
                mem.store(a, *ty, v);
            }
            Inst::MemCpy { dst, src, len } => {
                let d = self.rd(*dst, regs).as_i64() as u64;
                let s = self.rd(*src, regs).as_i64() as u64;
                mem.memcpy(d, s, *len);
            }
            Inst::MemZero { dst, len } => {
                let d = self.rd(*dst, regs).as_i64() as u64;
                mem.memzero(d, *len);
            }
            Inst::Prim {
                dst,
                prim,
                args,
                width,
            } => {
                let vals: Vec<RVal> = args.iter().map(|a| self.rd(*a, regs)).collect();
                let r = self.exec_prim(*prim, &vals, *width, mem)?;
                if let Some(d) = dst {
                    regs[*d as usize] = r;
                }
            }
            Inst::Call {
                dst,
                callee,
                args,
                sret,
                ..
            } => {
                let f = match callee {
                    Callee::Direct(name) => {
                        self.funcs.get(name.as_str()).copied().ok_or_else(|| {
                            CodegenError::new(format!("no such function: {name}"), None)
                        })?
                    }
                    Callee::Indirect(v) => {
                        let addr = self.rd(*v, regs).as_i64() as u64;
                        self.func_at(addr).ok_or_else(|| {
                            CodegenError::new("call through an invalid function pointer", None)
                        })?
                    }
                };
                // An aggregate return is delivered through the hidden leading `$sret`
                // pointer parameter.
                let mut argvals: Vec<RVal> = Vec::with_capacity(args.len() + 1);
                if let Some(s) = sret {
                    argvals.push(self.rd(*s, regs));
                }
                argvals.extend(args.iter().map(|a| self.rd(a.val, regs)));
                match self.exec_func(f, &argvals, mem)? {
                    Outcome::Returned(r) => {
                        if let Some(d) = dst {
                            regs[*d as usize] = r.unwrap_or(RVal::Int(0));
                        }
                    }
                    // The callee threw without catching: propagate to this frame's
                    // nearest `try` (handled by `run_blocks`).
                    Outcome::Threw => return Ok(InstFlow::Threw),
                }
            }
            other => {
                return Err(CodegenError::new(
                    format!("IR instruction not yet interpreted: {}", inst_name(other)),
                    None,
                ));
            }
        }
        Ok(InstFlow::Normal)
    }

    fn exec_prim(
        &self,
        prim: Prim,
        a: &[RVal],
        width: Option<Ty>,
        mem: &mut Mem,
    ) -> Result<RVal, CodegenError> {
        let res = match prim {
            Prim::StdWrite => {
                let fd = a[0].as_i64();
                let buf = a[1].as_i64() as u64;
                let n = a[2].as_i64().max(0) as usize;
                let bytes = mem.read_bytes(buf, n);
                match fd {
                    1 => mem.out.extend_from_slice(&bytes),
                    2 => {} // stderr: a side channel, not part of captured output
                    _ => return Ok(RVal::Int(-1)),
                }
                n as i64
            }
            Prim::MAlloc => mem.alloc_heap(a[0].as_i64()) as i64,
            Prim::Free => 0,
            Prim::MSize => {
                let addr = a[0].as_i64() as u64;
                mem.heap_sizes.get(&addr).copied().unwrap_or(0) as i64
            }
            // `HeapExtend(ptr, old, newsz)`: grow the last heap block in place, else
            // return NULL so `ReAlloc` falls back to allocate-copy-free.
            Prim::HeapExtend => {
                let ptr = a[0].as_i64() as u64;
                let newsz = a[2].as_i64().max(0);
                let oldsz = mem
                    .heap_sizes
                    .get(&ptr)
                    .copied()
                    .unwrap_or(a[1].as_i64().max(0) as u64);
                if ptr >= HEAP_BASE && ptr + oldsz == HEAP_BASE + mem.heap.len() as u64 {
                    let new_end = (ptr - HEAP_BASE) as usize + newsz as usize;
                    if mem.heap.len() < new_end {
                        mem.heap.resize(new_end, 0);
                    }
                    mem.heap_sizes.insert(ptr, newsz as u64);
                    ptr as i64
                } else {
                    0
                }
            }
            // ---- clock / time (impure) ----
            Prim::UnixNS => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0),
            Prim::NanoNS => {
                static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
                START
                    .get_or_init(std::time::Instant::now)
                    .elapsed()
                    .as_nanos() as i64
            }
            Prim::CpuNS => cpu_ns(),
            Prim::Sleep => {
                std::thread::sleep(std::time::Duration::from_nanos(a[0].as_i64().max(0) as u64));
                0
            }
            // ---- fd I/O (impure: a thin emulator over std::fs / std::net) ----
            Prim::Open => {
                let path = path_from_bytes(&mem.read_cstr(a[0].as_i64() as u64));
                let flags = a[1].as_i64();
                let mut opts = std::fs::OpenOptions::new();
                match flags & 0b11 {
                    0 => opts.read(true),
                    1 => opts.write(true),
                    _ => opts.read(true).write(true),
                };
                if flags & 0x40 != 0 {
                    opts.create(true);
                }
                if flags & 0x200 != 0 {
                    opts.truncate(true);
                }
                if flags & 0x400 != 0 {
                    opts.append(true);
                }
                if flags & 0x40 != 0 {
                    set_open_mode(&mut opts, a[2].as_i64() as u32);
                }
                match opts.open(path) {
                    Ok(file) => {
                        let fd = mem.next_fd;
                        mem.next_fd += 1;
                        mem.fds.insert(fd, FdObj::File(file));
                        fd
                    }
                    Err(e) => -norm_errno(e.raw_os_error().unwrap_or(2) as i64),
                }
            }
            Prim::LSeek => {
                let off = a[1].as_i64();
                let from = match a[2].as_i64() {
                    0 => std::io::SeekFrom::Start(off as u64),
                    1 => std::io::SeekFrom::Current(off),
                    2 => std::io::SeekFrom::End(off),
                    _ => return Ok(RVal::Int(-1)),
                };
                match mem.fds.get_mut(&a[0].as_i64()) {
                    Some(FdObj::File(f)) => {
                        std::io::Seek::seek(f, from).map(|p| p as i64).unwrap_or(-1)
                    }
                    _ => -1,
                }
            }
            Prim::Write => {
                let n = a[2].as_i64().max(0) as usize;
                let bytes = mem.read_bytes(a[1].as_i64() as u64, n);
                let r = match mem.fds.get_mut(&a[0].as_i64()) {
                    Some(FdObj::Tcp(s)) => std::io::Write::write(s, &bytes),
                    Some(FdObj::File(f)) => std::io::Write::write(f, &bytes),
                    _ => return Ok(RVal::Int(-1)),
                };
                r.map(|w| w as i64).unwrap_or(-1)
            }
            Prim::Read => {
                let n = a[2].as_i64().max(0) as usize;
                let mut buf = vec![0u8; n];
                let fd = a[0].as_i64();
                let got = if fd == 0 {
                    std::io::Read::read(&mut *mem.input, &mut buf)
                } else {
                    match mem.fds.get_mut(&fd) {
                        Some(FdObj::Tcp(s)) => std::io::Read::read(s, &mut buf),
                        Some(FdObj::File(f)) => std::io::Read::read(f, &mut buf),
                        _ => return Ok(RVal::Int(-1)),
                    }
                };
                match got {
                    Ok(cnt) => {
                        mem.write_bytes(a[1].as_i64() as u64, &buf[..cnt]);
                        cnt as i64
                    }
                    Err(_) => -1,
                }
            }
            Prim::Close => {
                mem.fds.remove(&a[0].as_i64());
                0
            }
            // ---- sockets (impure) ----
            Prim::Socket => {
                let fd = mem.next_fd;
                mem.next_fd += 1;
                mem.fds.insert(fd, FdObj::PendingSocket);
                fd
            }
            Prim::Connect => {
                let fd = a[0].as_i64();
                let sa = mem.read_bytes(a[1].as_i64() as u64, 16); // sockaddr_in
                let port = u16::from_be_bytes([sa[2], sa[3]]);
                let ip = std::net::Ipv4Addr::new(sa[4], sa[5], sa[6], sa[7]);
                if !matches!(mem.fds.get(&fd), Some(FdObj::PendingSocket)) {
                    -1
                } else {
                    match std::net::TcpStream::connect((ip, port)) {
                        Ok(s) => {
                            mem.fds.insert(fd, FdObj::Tcp(s));
                            0
                        }
                        Err(_) => -1,
                    }
                }
            }
            // ---- filesystem mutation / working directory (impure) ----
            Prim::Remove => {
                let p = path_from_bytes(&mem.read_cstr(a[0].as_i64() as u64));
                fs_result(std::fs::remove_file(p))
            }
            Prim::Rename => {
                let old = path_from_bytes(&mem.read_cstr(a[0].as_i64() as u64));
                let new = path_from_bytes(&mem.read_cstr(a[1].as_i64() as u64));
                fs_result(std::fs::rename(old, new))
            }
            Prim::Mkdir => {
                let p = path_from_bytes(&mem.read_cstr(a[0].as_i64() as u64));
                fs_result(mkdir_with_mode(&p, a[1].as_i64() as u32))
            }
            Prim::Chdir => {
                let p = path_from_bytes(&mem.read_cstr(a[0].as_i64() as u64));
                fs_result(std::env::set_current_dir(p))
            }
            Prim::Getcwd => {
                let cap = a[1].as_i64().max(0) as usize;
                match std::env::current_dir() {
                    Ok(dir) => {
                        let mut b = path_to_bytes(&dir);
                        b.push(0);
                        if b.len() > cap {
                            -34 // -ERANGE
                        } else {
                            mem.write_bytes(a[0].as_i64() as u64, &b);
                            0
                        }
                    }
                    Err(e) => -norm_errno(e.raw_os_error().unwrap_or(2) as i64),
                }
            }
            // ---- process / identity (impure) ----
            Prim::Getpid => std::process::id() as i64,
            Prim::Getppid => parent_pid() as i64,
            Prim::Getuid => get_uid() as i64,
            Prim::Getgid => get_gid() as i64,
            Prim::Exit => {
                mem.exit = Some(a[0].as_i64());
                return Err(CodegenError::new("program called Exit", None));
            }
            Prim::System => {
                // `sh -c cmd` (`cmd /C` on a Windows host), like the native targets.
                // The child's stdout is captured and appended to the program's
                // captured output (`mem.out`) so interp and native agree on the
                // observable bytes; its stderr stays a side channel, like StdWrite's.
                // Exit code 0–255, or -1 on abnormal exit — the backends' wait-status
                // decode.
                let cmd_bytes = mem.read_cstr(a[0].as_i64() as u64);
                let cmd = String::from_utf8_lossy(&cmd_bytes).into_owned();
                let (sh, flag) = if cfg!(windows) {
                    ("cmd", "/C")
                } else {
                    ("/bin/sh", "-c")
                };
                match std::process::Command::new(sh).arg(flag).arg(&cmd).output() {
                    Ok(out) => {
                        mem.out.extend_from_slice(&out.stdout);
                        out.status.code().unwrap_or(-1) as i64
                    }
                    Err(e) => -norm_errno(e.raw_os_error().unwrap_or(2) as i64),
                }
            }
            // ---- threads (run synchronously: body now, return stashed for Join) ----
            Prim::Thread => {
                let f = self
                    .func_at(a[0].as_i64() as u64)
                    .ok_or_else(|| CodegenError::new("Thread: not a function pointer", None))?;
                let rv = match self.exec_func(f, &[a[1]], mem) {
                    Ok(Outcome::Returned(v)) => v.map(|v| v.as_i64()).unwrap_or(0),
                    Ok(Outcome::Threw) => 0,
                    // A `ThreadExit(ret)` inside the body unwinds to here: the spawn
                    // catches it as the thread's join value (`thrd_exit` semantics).
                    Err(_) if mem.thread_exit.is_some() => mem.thread_exit.take().unwrap(),
                    Err(e) => return Err(e),
                };
                let handle = mem.next_thread;
                mem.next_thread += 1;
                mem.thread_results.insert(handle, rv);
                handle
            }
            Prim::Join => mem.thread_results.remove(&a[0].as_i64()).unwrap_or(0),
            Prim::ThreadYield => {
                std::thread::yield_now();
                0
            }
            Prim::ThreadExit => {
                mem.thread_exit = Some(a[0].as_i64());
                return Err(CodegenError::new("thread called ThreadExit", None));
            }
            // Detach forgets the stashed result: the handle can no longer be joined,
            // matching pthread_detach (joining a detached thread is undefined).
            Prim::ThreadDetach => {
                mem.thread_results.remove(&a[0].as_i64());
                0
            }
            // Threads run synchronously, so every body executes on the one real
            // thread; the process id is a stable, valid thread id for it.
            Prim::Gettid => std::process::id() as i64,
            // ---- atomics (single-threaded RMW; threads run synchronously) ----
            Prim::AtomicFence => 0,
            Prim::FutexWait | Prim::FutexWake => 0,
            // With synchronous threads nobody could ever wake a real sleep, so a
            // timed wait resolves immediately: a timeout when the value still
            // matches, else the futex EAGAIN ("value changed before sleeping").
            Prim::FutexWaitNs => {
                let addr = a[0].as_i64() as u64;
                let cur = i64::from(u32::from_le_bytes(
                    mem.read_bytes(addr, 4).try_into().unwrap(),
                ));
                let expected = a[1].as_i64() & 0xFFFF_FFFF;
                if cur == expected {
                    -110 // -ETIMEDOUT
                } else {
                    -11 // -EAGAIN
                }
            }
            Prim::AtomicLoad
            | Prim::AtomicStore
            | Prim::AtomicAdd
            | Prim::AtomicSwap
            | Prim::AtomicCas => exec_atomic(prim, a, width.unwrap_or(Ty::I64), mem),
            // ---- Win32 (`<windows.hc>`, modeled over std::fs; HANDLE == fd) ----
            Prim::WinCall { func } => exec_wincall(func, a, mem),
        };
        Ok(RVal::Int(res))
    }
}

/// Model a `Prim::WinCall` (a Win32 function from `<windows.hc>`) over the same fd
/// table the POSIX fd primitives use — a HANDLE is an fd integer. Faithful enough to
/// be the conformance oracle for the Windows backend's kernel32 lowering: file data
/// round-trips identically; only handle/error/pid *values* (never compared against a
/// real Windows host) differ. Out-parameters are written through the caller's pointer.
fn exec_wincall(func: &str, a: &[RVal], mem: &mut Mem) -> i64 {
    const GENERIC_READ: i64 = 0x8000_0000;
    const GENERIC_WRITE: i64 = 0x4000_0000;
    match func {
        // CreateFileA(name, access, share, sec, disposition, flags, template).
        "CreateFileA" => {
            let path = path_from_bytes(&mem.read_cstr(a[0].as_i64() as u64));
            let access = a[1].as_i64();
            let disposition = a[4].as_i64();
            let mut opts = std::fs::OpenOptions::new();
            opts.read(access & GENERIC_READ != 0);
            opts.write(access & GENERIC_WRITE != 0);
            match disposition {
                1 => {
                    opts.create_new(true);
                } // CREATE_NEW
                2 => {
                    opts.create(true).truncate(true);
                } // CREATE_ALWAYS
                4 => {
                    opts.create(true);
                } // OPEN_ALWAYS
                5 => {
                    opts.truncate(true);
                } // TRUNCATE_EXISTING
                _ => {} // OPEN_EXISTING (3) and others: open as-is
            }
            match opts.open(path) {
                Ok(file) => {
                    let fd = mem.next_fd;
                    mem.next_fd += 1;
                    mem.fds.insert(fd, FdObj::File(file));
                    fd // the HANDLE
                }
                Err(e) => {
                    mem.last_error = e.raw_os_error().unwrap_or(2) as u32;
                    -1 // INVALID_HANDLE_VALUE
                }
            }
        }
        // WriteFile(h, buf, n, &written, ovl) — write the DWORD count out-param.
        "WriteFile" => {
            let n = a[2].as_i64().max(0) as usize;
            let bytes = mem.read_bytes(a[1].as_i64() as u64, n);
            let r = match mem.fds.get_mut(&a[0].as_i64()) {
                Some(FdObj::File(f)) => std::io::Write::write(f, &bytes),
                Some(FdObj::Tcp(s)) => std::io::Write::write(s, &bytes),
                _ => {
                    mem.last_error = 6; // ERROR_INVALID_HANDLE
                    return 0;
                }
            };
            match r {
                Ok(w) => {
                    mem.write_bytes(a[3].as_i64() as u64, &(w as u32).to_le_bytes());
                    1 // BOOL TRUE
                }
                Err(_) => 0,
            }
        }
        // ReadFile(h, buf, n, &read, ovl) — write the DWORD count out-param.
        "ReadFile" => {
            let n = a[2].as_i64().max(0) as usize;
            let mut buf = vec![0u8; n];
            let got = match mem.fds.get_mut(&a[0].as_i64()) {
                Some(FdObj::File(f)) => std::io::Read::read(f, &mut buf),
                Some(FdObj::Tcp(s)) => std::io::Read::read(s, &mut buf),
                _ => {
                    mem.last_error = 6;
                    return 0;
                }
            };
            match got {
                Ok(cnt) => {
                    mem.write_bytes(a[1].as_i64() as u64, &buf[..cnt]);
                    mem.write_bytes(a[3].as_i64() as u64, &(cnt as u32).to_le_bytes());
                    1
                }
                Err(_) => 0,
            }
        }
        // SetFilePointerEx(h, distance, &newpos, method) — 8-byte position out-param.
        "SetFilePointerEx" => {
            let off = a[1].as_i64();
            let from = match a[3].as_i64() {
                0 => std::io::SeekFrom::Start(off as u64), // FILE_BEGIN
                1 => std::io::SeekFrom::Current(off),      // FILE_CURRENT
                2 => std::io::SeekFrom::End(off),          // FILE_END
                _ => return 0,
            };
            match mem.fds.get_mut(&a[0].as_i64()) {
                Some(FdObj::File(f)) => match std::io::Seek::seek(f, from) {
                    Ok(p) => {
                        // lpNewFilePointer may be NULL.
                        if a[2].as_i64() != 0 {
                            mem.write_bytes(a[2].as_i64() as u64, &(p as i64).to_le_bytes());
                        }
                        1
                    }
                    Err(_) => 0,
                },
                _ => 0,
            }
        }
        // GetFileSizeEx(h, &size) — 8-byte size out-param.
        "GetFileSizeEx" => match mem.fds.get(&a[0].as_i64()) {
            Some(FdObj::File(f)) => match f.metadata() {
                Ok(m) => {
                    mem.write_bytes(a[1].as_i64() as u64, &(m.len() as i64).to_le_bytes());
                    1
                }
                Err(_) => 0,
            },
            _ => 0,
        },
        "CloseHandle" => {
            mem.fds.remove(&a[0].as_i64());
            1
        }
        "GetLastError" => mem.last_error as i64,
        "GetCurrentProcessId" => std::process::id() as i64,
        // --- registry (advapi32), modeled over the in-memory store ----------------
        // RegCreateKeyExA(key, subkey, _, class, options, sam, sec, &result, &disp).
        "RegCreateKeyExA" => {
            let Some(parent) = mem.reg_open.get(&a[0].as_i64()).cloned() else {
                return 6; // ERROR_INVALID_HANDLE
            };
            let sub = String::from_utf8_lossy(&mem.read_cstr(a[1].as_i64() as u64)).into_owned();
            let full = format!("{parent}\\{sub}");
            let handle = mem.next_hkey;
            mem.next_hkey += 1;
            mem.reg_open.insert(handle, full);
            mem.write_bytes(a[7].as_i64() as u64, &handle.to_le_bytes()); // *result = HKEY
            if a[8].as_i64() != 0 {
                mem.write_bytes(a[8].as_i64() as u64, &1u32.to_le_bytes()); // REG_CREATED_NEW_KEY
            }
            0 // ERROR_SUCCESS
        }
        // RegSetValueExA(key, name, _, type, data, cbdata).
        "RegSetValueExA" => {
            let Some(path) = mem.reg_open.get(&a[0].as_i64()).cloned() else {
                return 6;
            };
            let name = String::from_utf8_lossy(&mem.read_cstr(a[1].as_i64() as u64)).into_owned();
            let ty = a[3].as_i64() as u32;
            let bytes = mem.read_bytes(a[4].as_i64() as u64, a[5].as_i64().max(0) as usize);
            mem.reg_values.insert((path, name), (bytes, ty));
            0
        }
        // RegQueryValueExA(key, name, _, &type, data, &cbdata). `cbdata` is in/out.
        "RegQueryValueExA" => {
            let Some(path) = mem.reg_open.get(&a[0].as_i64()).cloned() else {
                return 6;
            };
            let name = String::from_utf8_lossy(&mem.read_cstr(a[1].as_i64() as u64)).into_owned();
            let Some((bytes, ty)) = mem.reg_values.get(&(path, name)).cloned() else {
                return 2; // ERROR_FILE_NOT_FOUND
            };
            if a[3].as_i64() != 0 {
                mem.write_bytes(a[3].as_i64() as u64, &ty.to_le_bytes()); // *type
            }
            let cap =
                u32::from_le_bytes(mem.read_bytes(a[5].as_i64() as u64, 4).try_into().unwrap())
                    as usize;
            mem.write_bytes(a[5].as_i64() as u64, &(bytes.len() as u32).to_le_bytes()); // *cbdata = actual
            if a[4].as_i64() != 0 {
                // data buffer given
                if bytes.len() > cap {
                    return 234; // ERROR_MORE_DATA
                }
                mem.write_bytes(a[4].as_i64() as u64, &bytes);
            }
            0
        }
        "RegCloseKey" => {
            // Predefined roots (negative handles) stay open; close only opened keys.
            if a[0].as_i64() >= 0 {
                mem.reg_open.remove(&a[0].as_i64());
            }
            0
        }
        // RegDeleteKeyA(key, subkey) — drop the subkey and its values.
        "RegDeleteKeyA" => {
            let Some(parent) = mem.reg_open.get(&a[0].as_i64()).cloned() else {
                return 6;
            };
            let sub = String::from_utf8_lossy(&mem.read_cstr(a[1].as_i64() as u64)).into_owned();
            let full = format!("{parent}\\{sub}");
            mem.reg_values
                .retain(|(p, _), _| *p != full && !p.starts_with(&format!("{full}\\")));
            0
        }
        // win_import gates the set, so this is unreachable for a real call.
        _ => 0,
    }
}

/// `0` on success, `-errno` (Linux-normalised) on failure — the fd/fs syscall contract.
fn fs_result(r: std::io::Result<()>) -> i64 {
    match r {
        Ok(()) => 0,
        Err(e) => -norm_errno(e.raw_os_error().unwrap_or(2) as i64),
    }
}

/// A single-threaded atomic read-modify-write of the `width`-sized scalar at `a[0]`.
/// Threads run synchronously here, so there is no contention; the load/store widths and
/// extensions match the native hardware-atomic lowering.
fn exec_atomic(prim: Prim, a: &[RVal], width: Ty, mem: &mut Mem) -> i64 {
    let addr = a[0].as_i64() as u64;
    let old = mem.load(addr, width).as_i64();
    match prim {
        Prim::AtomicLoad => old,
        Prim::AtomicStore => {
            mem.store(addr, width, RVal::Int(a[1].as_i64()));
            0
        }
        Prim::AtomicAdd => {
            mem.store(addr, width, RVal::Int(old.wrapping_add(a[1].as_i64())));
            mem.load(addr, width).as_i64() // the new (stored-width) value
        }
        Prim::AtomicSwap => {
            mem.store(addr, width, RVal::Int(a[1].as_i64()));
            old
        }
        Prim::AtomicCas => {
            if old == a[1].as_i64() {
                mem.store(addr, width, RVal::Int(a[2].as_i64()));
            }
            old
        }
        _ => unreachable!(),
    }
}

/// Arithmetic / bitwise / shift over the `signed`/`ty` that lowering froze into the IR op
/// (never re-derived from the operand values), so the backends can't compute it differently.
/// `pub(crate)` so the backend's constant-folding pass ([`crate::backend`]'s `simplify`) folds
/// a constant `Bin` through the *same* arithmetic the oracle runs — bit-identical by construction.
pub(crate) fn bin(op: BinOp, ty: Ty, signed: bool, l: RVal, r: RVal) -> Result<RVal, CodegenError> {
    use BinOp::*;
    if ty.is_float() {
        let a = l.as_f64();
        let b = r.as_f64();
        let v = match op {
            Add => a + b,
            Sub => a - b,
            Mul => a * b,
            Div => a / b,
            // The truncated remainder `a - trunc(a/b)*b` (HolyC's documented `Fmod` form),
            // NOT Rust's exact `a % b`. `f64::trunc` is magnitude-safe, matching the
            // backends' `frintz` / sentinel-guarded `cvttsd2si`, so `%` and `Fmod()` agree.
            Mod => a - (a / b).trunc() * b,
            BitAnd | BitOr | BitXor | Shl | Shr => {
                return Err(CodegenError::new("bitwise op on a float", None));
            }
        };
        return Ok(RVal::Float(v));
    }
    let a = l.as_i64();
    let b = r.as_i64();
    let zero = || CodegenError::new("division by zero", None);
    let v = match op {
        Add => a.wrapping_add(b),
        Sub => a.wrapping_sub(b),
        Mul => a.wrapping_mul(b),
        Div => {
            if b == 0 {
                return Err(zero());
            }
            if signed {
                a.wrapping_div(b)
            } else {
                ((a as u64) / (b as u64)) as i64
            }
        }
        Mod => {
            if b == 0 {
                return Err(zero());
            }
            if signed {
                a.wrapping_rem(b)
            } else {
                ((a as u64) % (b as u64)) as i64
            }
        }
        BitAnd => a & b,
        BitOr => a | b,
        BitXor => a ^ b,
        Shl => a.wrapping_shl(b as u32),
        Shr if signed => a.wrapping_shr(b as u32),
        Shr => (a as u64).wrapping_shr(b as u32) as i64,
    };
    Ok(RVal::Int(v))
}

/// Comparison over the frozen `signed`/`ty` the IR `Cmp` carries (full 64-bit integer
/// compares, not via `f64`); the relational signedness was decided once at lowering.
/// `pub(crate)` so the backend's constant-folding pass folds a constant `Cmp` identically.
pub(crate) fn cmp(op: CmpOp, ty: Ty, signed: bool, l: RVal, r: RVal) -> bool {
    if ty.is_float() {
        let a = l.as_f64();
        let b = r.as_f64();
        return match op {
            CmpOp::Eq => a == b,
            CmpOp::Ne => a != b,
            CmpOp::Lt => a < b,
            CmpOp::Le => a <= b,
            CmpOp::Gt => a > b,
            CmpOp::Ge => a >= b,
        };
    }
    let a = l.as_i64();
    let b = r.as_i64();
    match op {
        CmpOp::Eq => a == b,
        CmpOp::Ne => a != b,
        _ if signed => match op {
            CmpOp::Lt => a < b,
            CmpOp::Le => a <= b,
            CmpOp::Gt => a > b,
            CmpOp::Ge => a >= b,
            _ => unreachable!(),
        },
        _ => {
            let (a, b) = (a as u64, b as u64);
            match op {
                CmpOp::Lt => a < b,
                CmpOp::Le => a <= b,
                CmpOp::Gt => a > b,
                CmpOp::Ge => a >= b,
                _ => unreachable!(),
            }
        }
    }
}

/// A type cast, dispatched on the `Cast{from,to}` IR fields lowering froze in.
fn cast(to: Ty, v: RVal) -> RVal {
    let i: i64 = match v {
        RVal::Float(f) if matches!(to, Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64) => f as u64 as i64,
        _ => v.as_i64(),
    };
    match to {
        Ty::F64 => RVal::Float(v.as_f64()),
        Ty::I8 => RVal::Int(i as i8 as i64),
        Ty::U8 => RVal::Int(i & 0xFF),
        Ty::I16 => RVal::Int(i as i16 as i64),
        Ty::U16 => RVal::Int(i & 0xFFFF),
        Ty::I32 => RVal::Int(i as i32 as i64),
        Ty::U32 => RVal::Int(i & 0xFFFF_FFFF),
        Ty::I64 | Ty::U64 => RVal::Int(i),
        Ty::Ptr => v,
    }
}

fn inst_name(inst: &Inst) -> &'static str {
    match inst {
        Inst::GlobalAddr { .. } => "globaladdr",
        Inst::FuncAddr { .. } => "funcaddr",
        Inst::TryBegin { .. } => "trybegin",
        Inst::TryEnd => "tryend",
        _ => "instruction",
    }
}

// ---- public entry points ----
//
// Lower a parsed, type-checked program to SSA IR ([`crate::lower`]) and run it on
// [`Oracle`] (the conformance oracle). Run semantic analysis and layout first; the
// oracle assumes a well-formed program and only reports faults it hits at run time
// (division by zero, a null dereference, a missing function body). HolyC's implicit print
// is honoured by the lowering: a bare string-literal statement prints itself, and
// `"fmt", args…` formats and prints.

/// Run a program and capture its output, lossily decoded as UTF-8. Convenient for tests
/// whose expected output is text; use [`run_to_bytes`] when comparing against a native
/// binary, since a HolyC program can emit non-UTF-8 bytes that this would mangle.
pub fn run_to_string(program: &crate::ast::Program) -> Result<String, CodegenError> {
    run_to_bytes(program).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// [`run_to_string`] with `input` on the program's standard input (fd 0).
pub fn run_to_string_with_input(
    program: &crate::ast::Program,
    input: &[u8],
) -> Result<String, CodegenError> {
    run_to_bytes_with_input(program, input).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Run a program and capture its **raw** stdout bytes via the SSA IR interpreter (the
/// conformance oracle): lower to IR ([`crate::lower`]) and execute it here. This is the
/// byte-exact output a native binary must reproduce — unlike [`run_to_string`], it does
/// not lossy-decode, so non-UTF-8 output (`"%c", 0xFF`, binary data) is preserved.
pub fn run_to_bytes(program: &crate::ast::Program) -> Result<Vec<u8>, CodegenError> {
    run_to_bytes_with_input(program, &[])
}

/// [`run_to_bytes`] with `input` on the program's standard input (fd 0).
pub fn run_to_bytes_with_input(
    program: &crate::ast::Program,
    input: &[u8],
) -> Result<Vec<u8>, CodegenError> {
    run_to_bytes_with(program, &[], input)
}

/// [`run_to_bytes`] with a full command line and standard input: `args` is the program's
/// `argv` (including `argv[0]`, the program name) and `input` feeds fd 0. An empty `args`
/// keeps the interpreter's default (`["hcc"]`). This is the entry point the integration
/// tests use so a case's `//@ args:`/`//@ stdin:` directives reach the oracle the same way
/// they reach a native binary's `argv`/stdin.
pub fn run_to_bytes_with(
    program: &crate::ast::Program,
    args: &[String],
    input: &[u8],
) -> Result<Vec<u8>, CodegenError> {
    if let Some(e) = crate::sema::check_program(program).into_iter().next() {
        return Err(CodegenError::at(
            e.pos,
            format!("semantic error: {}", e.message),
        ));
    }
    let (layouts, layout_errs) = crate::layout::compute(program);
    if let Some(e) = layout_errs.into_iter().next() {
        return Err(CodegenError::at(
            e.pos,
            format!("layout error: {}", e.message),
        ));
    }
    let ir = crate::lower::lower(program, &layouts)?;
    let mut interp = Oracle::new(&ir);
    interp.set_args(args.to_vec());
    interp.set_input(Box::new(std::io::Cursor::new(input.to_vec())));
    interp.run_program()
}

/// Darwin → Linux `errno` remaps, as `(darwin, linux)` pairs, for the codes that can
/// reach a `-errno` return on the Darwin backend — the filesystem ops
/// (`Open`/`Remove`/`Rename`/`Mkdir`/`Chdir`/`Getcwd`). The fd and socket ops surface
/// a plain `-1` on Darwin today (not `-errno`), so the networking codes
/// (`ECONNREFUSED`, `ETIMEDOUT`, …) never flow through normalization and are omitted.
///
/// The overwhelming majority of file-domain codes already agree across the two systems
/// (`ENOENT` 2, `EACCES` 13, `EEXIST` 17, `EINVAL` 22, `EISDIR` 21, `ENOTDIR` 20,
/// `ENOSPC` 28, `EROFS` 30, `EMFILE` 24, `ENFILE` 23, `EFBIG` 27, …); only these few
/// differ. The values are the Linux-canonical ones the `lib/errno.hc` constants use.
/// Both the interpreter ([`darwin_to_linux_errno`]) and the AArch64 Darwin backend
/// (which emits a matching compare-chain) read this one table, so they cannot drift.
pub const DARWIN_TO_LINUX_ERRNO: &[(i64, i64)] = &[
    (35, 11), // EAGAIN / EWOULDBLOCK
    (11, 35), // EDEADLK
    (63, 36), // ENAMETOOLONG
    (62, 40), // ELOOP
    (66, 39), // ENOTEMPTY
];

/// Translate a positive Darwin `errno` to its Linux-canonical value, or return it
/// unchanged when the two systems already agree. See [`DARWIN_TO_LINUX_ERRNO`].
pub fn darwin_to_linux_errno(d: i64) -> i64 {
    DARWIN_TO_LINUX_ERRNO
        .iter()
        .find(|&&(darwin, _)| darwin == d)
        .map(|&(_, linux)| linux)
        .unwrap_or(d)
}

// ---- cross-platform OS shims ----
//
// The interpreter emulates HolyC's POSIX-flavoured fd/file/process primitives over
// `std`. HolyC paths arrive as raw NUL-terminated byte strings, and file ops carry Unix
// mode bits. The few spots that need Unix-only `std` APIs are funneled through these
// shims, so the tools also build and run on non-Unix hosts (Windows). There, the mode
// bits are ignored and there is no POSIX uid/gid.

/// An interpreter file descriptor: a reserved-but-unconnected socket, a live TCP stream,
/// or an open file. `Read`/`Write` go to the stream or file; `LSeek` works only on a file.
enum FdObj {
    PendingSocket,
    Tcp(std::net::TcpStream),
    File(std::fs::File),
}

/// Build a filesystem path from HolyC's raw path bytes. On Unix the bytes pass straight
/// through (`OsStr::from_bytes`). Elsewhere they are read as UTF-8, which is fine because
/// HolyC paths are ASCII in practice.
#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    Path::new(std::ffi::OsStr::from_bytes(bytes)).to_path_buf()
}
#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// The raw bytes of a path (the inverse of [`path_from_bytes`]).
#[cfg(unix)]
fn path_to_bytes(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().to_vec()
}
#[cfg(not(unix))]
fn path_to_bytes(p: &Path) -> Vec<u8> {
    p.to_string_lossy().into_owned().into_bytes()
}

/// Apply a Unix permission `mode` to a file being created. A no-op on platforms without
/// Unix mode bits (Windows).
#[cfg(unix)]
fn set_open_mode(opts: &mut std::fs::OpenOptions, mode: u32) {
    use std::os::unix::fs::OpenOptionsExt;
    opts.mode(mode);
}
#[cfg(not(unix))]
fn set_open_mode(_opts: &mut std::fs::OpenOptions, _mode: u32) {}

/// `mkdir(path, mode)`. The `mode` is applied on Unix and ignored elsewhere.
#[cfg(unix)]
fn mkdir_with_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(mode).create(path)
}
#[cfg(not(unix))]
fn mkdir_with_mode(path: &Path, _mode: u32) -> std::io::Result<()> {
    std::fs::DirBuilder::new().create(path)
}

/// Normalise a host OS `errno` to the Linux-canonical numbering the `<errno.hc>` constants
/// use. On a Linux host `raw_os_error()` is already canonical (identity); on macOS the
/// Darwin codes that differ are remapped, so the interpreter (the oracle) agrees with what
/// the Darwin native binary returns (which remaps the same way). See
/// [`crate::intrinsics::DARWIN_TO_LINUX_ERRNO`].
#[cfg(target_os = "macos")]
fn norm_errno(raw: i64) -> i64 {
    crate::intrinsics::darwin_to_linux_errno(raw)
}
#[cfg(not(target_os = "macos"))]
fn norm_errno(raw: i64) -> i64 {
    raw
}

/// The parent process id, or 0 where `std` can't report it portably, i.e. Windows.
#[cfg(unix)]
fn parent_pid() -> u32 {
    std::os::unix::process::parent_id()
}
#[cfg(not(unix))]
fn parent_pid() -> u32 {
    0
}

/// The real user/group id, read via libc on Unix. Returns 0 where there is no POSIX
/// uid/gid.
#[cfg(unix)]
fn get_uid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}
#[cfg(unix)]
fn get_gid() -> u32 {
    unsafe extern "C" {
        fn getgid() -> u32;
    }
    unsafe { getgid() }
}
#[cfg(not(unix))]
fn get_uid() -> u32 {
    0
}
#[cfg(not(unix))]
fn get_gid() -> u32 {
    0
}

/// Process CPU time in nanoseconds (`CpuNS`), via libc `clock_gettime` over the
/// process-CPU-time clock. There is no `std` accessor for it. The clock id differs by host
/// (Linux 2, macOS 12). Returns 0 where there is no POSIX CPU clock.
#[cfg(unix)]
fn cpu_ns() -> i64 {
    #[cfg(target_os = "macos")]
    const CPU_CLOCK: i32 = 12; // CLOCK_PROCESS_CPUTIME_ID on macOS
    #[cfg(not(target_os = "macos"))]
    const CPU_CLOCK: i32 = 2; // CLOCK_PROCESS_CPUTIME_ID on Linux
    #[repr(C)]
    struct Ts {
        sec: i64,
        nsec: i64,
    }
    unsafe extern "C" {
        fn clock_gettime(id: i32, ts: *mut Ts) -> i32;
    }
    let mut ts = Ts { sec: 0, nsec: 0 };
    unsafe { clock_gettime(CPU_CLOCK, &mut ts) };
    ts.sec * 1_000_000_000 + ts.nsec
}
#[cfg(not(unix))]
fn cpu_ns() -> i64 {
    0
}

#[cfg(test)]
#[path = "tests/oracle.rs"]
mod tests;
