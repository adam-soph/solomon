//! A tree-of-blocks interpreter for the SSA [IR](crate::ir).
//!
//! This is the future conformance oracle: it executes [`IrProgram`] directly, where
//! today [`crate::interp`] tree-walks the AST. It covers the deterministic language:
//! scalar and aggregate computation, control flow (incl. `switch`), direct/indirect
//! calls and aggregate-by-value (sret), the pure-HolyC printf path (varargs), globals,
//! string literals, exceptions (`try`/`throw` over `Fs`), and the heap/`StdWrite`
//! primitives. It matches the AST oracle byte-for-byte on the deterministic corpus
//! (see `tests/ir_diff.rs`).
//!
//! Memory is three disjoint regions in one flat address space (the "what GCC does"
//! model: pointers are real addresses): a per-call **stack** (bump-allocated, reclaimed
//! on return), a read-mostly **data** region for string literals and globals, and a
//! **heap** for `MAlloc`. Functions occupy a fourth synthetic range so `&Func` and
//! indirect calls work. Arithmetic, comparison, and casts mirror [`crate::interp`]'s
//! `apply_binop`/`cast_value` bit-for-bit.
//!
//! Not yet handled: the impure primitives (clock, fd/file I/O, sockets, threads,
//! atomics) and the command line (`ArgC`/`ArgV`/`EnvP`) — these are property-tested,
//! not value-pinned, against the oracle.

use std::collections::HashMap;

use crate::codegen::CodegenError;
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
    fds: HashMap<i64, crate::interp::FdObj>,
    next_fd: i64,
    /// `Thread`/`Join` run synchronously (the function body runs at spawn and its
    /// return is stashed here for `Join`), matching the tree-walking interpreter.
    thread_results: HashMap<i64, i64>,
    next_thread: i64,
    /// The program's standard input (fd 0).
    input: Box<dyn std::io::Read>,
    /// Set by `Exit(code)`; unwinds the run and finishes with the output so far.
    exit: Option<i64>,
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

    fn load(&self, addr: u64, ty: IrTy) -> RVal {
        let bytes = self.read_bytes(addr, ty.size() as usize);
        let mut raw = [0u8; 8];
        raw[..bytes.len()].copy_from_slice(&bytes);
        let u = u64::from_le_bytes(raw);
        match ty {
            IrTy::F64 => RVal::Float(f64::from_bits(u)),
            IrTy::I8 => RVal::Int(u as u8 as i8 as i64),
            IrTy::U8 => RVal::Int(u as u8 as i64),
            IrTy::I16 => RVal::Int(u as u16 as i16 as i64),
            IrTy::U16 => RVal::Int(u as u16 as i64),
            IrTy::I32 => RVal::Int(u as u32 as i32 as i64),
            IrTy::U32 => RVal::Int(u as u32 as i64),
            IrTy::I64 | IrTy::U64 | IrTy::Ptr => RVal::Int(u as i64),
        }
    }

    fn store(&mut self, addr: u64, ty: IrTy, v: RVal) {
        let n = ty.size() as usize;
        let bits: u64 = match ty {
            IrTy::F64 => v.as_f64().to_bits(),
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

/// An IR interpreter over one program. Holds the static data image (string literals
/// and globals) laid out once; each run seeds a fresh [`Mem`] from it.
pub struct IrInterp<'p> {
    prog: &'p IrProgram,
    funcs: HashMap<&'p str, &'p IrFunc>,
    /// Functions by synthetic address index, for indirect calls (`FUNC_BASE + i`).
    func_list: Vec<&'p IrFunc>,
    func_index: HashMap<&'p str, usize>,
    data: Vec<u8>,
    str_addr: Vec<u64>,
    global_addr: Vec<u64>,
    /// Command-line arguments exposed via `ArgC`/`ArgV` (`args[0]` is the program name,
    /// so the count is always ≥ 1).
    args: Vec<String>,
    /// The program's standard input (fd 0), consumed on the next `run`/`run_program`.
    input: std::cell::RefCell<Option<Box<dyn std::io::Read>>>,
}

impl<'p> IrInterp<'p> {
    pub fn new(prog: &'p IrProgram) -> Self {
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
        let func_list: Vec<&IrFunc> = prog.funcs.iter().collect();
        let func_index = func_list
            .iter()
            .enumerate()
            .map(|(i, f)| (f.name.as_str(), i))
            .collect();
        IrInterp {
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

    /// Set the command line visible through `ArgC`/`ArgV` (`args[0]` = program name).
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
    fn func_at(&self, addr: u64) -> Option<&'p IrFunc> {
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
            mem.store(self.global_addr[idx], IrTy::Ptr, RVal::Int(task as i64));
        }
        self.seed_command_line(&mut mem);
        mem
    }

    /// Seed `ArgC`/`ArgV`/`EnvP` (when the program registered them) to the configured
    /// command line (`args[0]` is the program name) and the real process environment —
    /// matching the tree-walking interpreter and the native backends.
    fn seed_command_line(&self, mem: &mut Mem) {
        let store_global = |mem: &mut Mem, name: &str, v: i64| {
            if let Some(idx) = self.prog.globals.iter().position(|g| g.name == name) {
                mem.store(self.global_addr[idx], IrTy::Ptr, RVal::Int(v));
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
                mem.store(base + (i * 8) as u64, IrTy::Ptr, RVal::Int(p as i64));
            }
            mem.store(base + (items.len() * 8) as u64, IrTy::Ptr, RVal::Int(0));
            base
        };
        if store_global(mem, "ArgC", self.args.len() as i64) {
            let argv: Vec<Vec<u8>> = self.args.iter().map(|a| a.as_bytes().to_vec()).collect();
            let base = make_argv(mem, &argv);
            store_global(mem, "ArgV", base as i64);
        }
        if self.prog.globals.iter().any(|g| g.name == "EnvP") {
            let env: Vec<Vec<u8>> = std::env::vars_os()
                .map(|(k, v)| {
                    let mut s = k.into_encoded_bytes();
                    s.push(b'=');
                    s.extend_from_slice(&v.into_encoded_bytes());
                    s
                })
                .collect();
            let base = make_argv(mem, &env);
            store_global(mem, "EnvP", base as i64);
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
            Err(_) if mem.exit.is_some() => Ok(None), // `Exit` unwinds cleanly
            Err(e) => Err(e),
        }
    }

    /// Run the synthesised top-level entry, returning the captured `StdWrite`-to-fd-1
    /// output as a string (the analogue of `interp::run_to_string`). An uncaught throw —
    /// or `Exit` — finishes cleanly after the output so far, matching the oracle.
    pub fn run_program(&self) -> Result<String, CodegenError> {
        let mut mem = self.fresh_mem();
        if let Some(f) = self.funcs.get(crate::lower::ENTRY) {
            match self.exec_func(f, &[], &mut mem) {
                Ok(_) => {}
                Err(_) if mem.exit.is_some() => {} // `Exit`: finish with the output so far
                Err(e) => return Err(e),
            }
        }
        Ok(String::from_utf8_lossy(&mem.out).into_owned())
    }

    fn exec_func(&self, f: &IrFunc, args: &[RVal], mem: &mut Mem) -> Result<Outcome, CodegenError> {
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
        f: &IrFunc,
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
                    IrInst::TryBegin { pad, .. } => try_stack.push(*pad),
                    IrInst::TryEnd => {
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
                IrTerm::Br(t) => {
                    prev = Some(cur);
                    cur = *t;
                }
                IrTerm::CondBr { cond, t, f: fb } => {
                    let take = self.eval_cond(cond, regs);
                    prev = Some(cur);
                    cur = if take { *t } else { *fb };
                }
                IrTerm::Switch {
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
                IrTerm::Ret(v) => return Ok(Outcome::Returned(v.map(|v| self.rd(v, regs)))),
                // The throw value and `Fs` flags were written by the preceding stores;
                // here we only transfer control to the nearest handler, else unwind.
                IrTerm::Throw(_) | IrTerm::Rethrow => match try_stack.pop() {
                    Some(pad) => {
                        prev = Some(cur);
                        cur = pad;
                    }
                    None => return Ok(Outcome::Threw),
                },
                IrTerm::Unreachable => {
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
        inst: &IrInst,
        regs: &mut [RVal],
        slot_base: &[u64],
        mem: &mut Mem,
    ) -> Result<InstFlow, CodegenError> {
        match inst {
            IrInst::Bin {
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
            IrInst::Un { dst, op, ty, src } => {
                let v = self.rd(*src, regs);
                regs[*dst as usize] = match op {
                    IrUnOp::Neg => {
                        if ty.is_float() {
                            RVal::Float(-v.as_f64())
                        } else {
                            RVal::Int(v.as_i64().wrapping_neg())
                        }
                    }
                    IrUnOp::BitNot => RVal::Int(!v.as_i64()),
                };
            }
            IrInst::Cast { dst, to, src, .. } => {
                let v = self.rd(*src, regs);
                regs[*dst as usize] = cast(*to, v);
            }
            IrInst::Mov { dst, src, .. } => {
                regs[*dst as usize] = self.rd(*src, regs);
            }
            IrInst::Cmp {
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
            IrInst::SlotAddr { dst, slot, off } => {
                regs[*dst as usize] = RVal::Int(slot_base[*slot as usize] as i64 + *off as i64);
            }
            IrInst::StrAddr { dst, str } => {
                regs[*dst as usize] = RVal::Int(self.str_addr[*str as usize] as i64);
            }
            IrInst::GlobalAddr { dst, global, off } => {
                regs[*dst as usize] =
                    RVal::Int(self.global_addr[*global as usize] as i64 + *off as i64);
            }
            IrInst::FuncAddr { dst, func } => {
                let idx = self.func_index.get(func.as_str()).ok_or_else(|| {
                    CodegenError::new(format!("address of unknown function: {func}"), None)
                })?;
                regs[*dst as usize] = RVal::Int((FUNC_BASE + *idx as u64) as i64);
            }
            IrInst::PtrAdd {
                dst,
                base,
                index,
                stride,
            } => {
                let b = self.rd(*base, regs).as_i64();
                let i = self.rd(*index, regs).as_i64();
                regs[*dst as usize] = RVal::Int(b + i * (*stride as i64));
            }
            IrInst::Load { dst, ty, addr } => {
                let a = self.rd(*addr, regs).as_i64() as u64;
                regs[*dst as usize] = mem.load(a, *ty);
            }
            IrInst::Store { ty, addr, val } => {
                let a = self.rd(*addr, regs).as_i64() as u64;
                let v = self.rd(*val, regs);
                mem.store(a, *ty, v);
            }
            IrInst::MemCpy { dst, src, len } => {
                let d = self.rd(*dst, regs).as_i64() as u64;
                let s = self.rd(*src, regs).as_i64() as u64;
                mem.memcpy(d, s, *len);
            }
            IrInst::MemZero { dst, len } => {
                let d = self.rd(*dst, regs).as_i64() as u64;
                mem.memzero(d, *len);
            }
            IrInst::Prim {
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
            IrInst::Call {
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
        width: Option<IrTy>,
        mem: &mut Mem,
    ) -> Result<RVal, CodegenError> {
        use crate::interp::{
            FdObj, cpu_ns, get_gid, get_uid, mkdir_with_mode, norm_errno, parent_pid,
            path_from_bytes, path_to_bytes, set_open_mode,
        };
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
            // ---- threads (run synchronously: body now, return stashed for Join) ----
            Prim::Thread => {
                let f = self
                    .func_at(a[0].as_i64() as u64)
                    .ok_or_else(|| CodegenError::new("Thread: not a function pointer", None))?;
                let rv = match self.exec_func(f, &[a[1]], mem)? {
                    Outcome::Returned(v) => v.map(|v| v.as_i64()).unwrap_or(0),
                    Outcome::Threw => 0,
                };
                let handle = mem.next_thread;
                mem.next_thread += 1;
                mem.thread_results.insert(handle, rv);
                handle
            }
            Prim::Join => mem.thread_results.remove(&a[0].as_i64()).unwrap_or(0),
            // ---- atomics (single-threaded RMW; threads run synchronously) ----
            Prim::AtomicFence => 0,
            Prim::FutexWait | Prim::FutexWake => 0,
            Prim::AtomicLoad
            | Prim::AtomicStore
            | Prim::AtomicAdd
            | Prim::AtomicSwap
            | Prim::AtomicCas => exec_atomic(prim, a, width.unwrap_or(IrTy::I64), mem),
        };
        Ok(RVal::Int(res))
    }
}

/// `0` on success, `-errno` (Linux-normalised) on failure — the fd/fs syscall contract.
fn fs_result(r: std::io::Result<()>) -> i64 {
    match r {
        Ok(()) => 0,
        Err(e) => -crate::interp::norm_errno(e.raw_os_error().unwrap_or(2) as i64),
    }
}

/// A single-threaded atomic read-modify-write of the `width`-sized scalar at `a[0]`.
/// Threads run synchronously here, so there is no contention; the load/store widths and
/// extensions match the native hardware-atomic lowering.
fn exec_atomic(prim: Prim, a: &[RVal], width: IrTy, mem: &mut Mem) -> i64 {
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

/// Arithmetic / bitwise / shift, mirroring `interp::apply_binop`'s numeric core.
fn bin(op: IrBinOp, ty: IrTy, signed: bool, l: RVal, r: RVal) -> Result<RVal, CodegenError> {
    use IrBinOp::*;
    if ty.is_float() {
        let a = l.as_f64();
        let b = r.as_f64();
        let v = match op {
            Add => a + b,
            Sub => a - b,
            Mul => a * b,
            Div => a / b,
            Mod => a % b,
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

/// Comparison, mirroring `interp::apply_binop`'s relational/equality core.
fn cmp(op: CmpOp, ty: IrTy, signed: bool, l: RVal, r: RVal) -> bool {
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

/// A type cast, mirroring `interp::cast_value`.
fn cast(to: IrTy, v: RVal) -> RVal {
    let i: i64 = match v {
        RVal::Float(f) if matches!(to, IrTy::U8 | IrTy::U16 | IrTy::U32 | IrTy::U64) => {
            f as u64 as i64
        }
        _ => v.as_i64(),
    };
    match to {
        IrTy::F64 => RVal::Float(v.as_f64()),
        IrTy::I8 => RVal::Int(i as i8 as i64),
        IrTy::U8 => RVal::Int(i & 0xFF),
        IrTy::I16 => RVal::Int(i as i16 as i64),
        IrTy::U16 => RVal::Int(i & 0xFFFF),
        IrTy::I32 => RVal::Int(i as i32 as i64),
        IrTy::U32 => RVal::Int(i & 0xFFFF_FFFF),
        IrTy::I64 | IrTy::U64 => RVal::Int(i),
        IrTy::Ptr => v,
    }
}

fn inst_name(inst: &IrInst) -> &'static str {
    match inst {
        IrInst::GlobalAddr { .. } => "globaladdr",
        IrInst::FuncAddr { .. } => "funcaddr",
        IrInst::TryBegin { .. } => "trybegin",
        IrInst::TryEnd => "tryend",
        _ => "instruction",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lower_program(src: &str) -> Result<IrProgram, String> {
        let prog = crate::parser::parse(src).map_err(|e| format!("parse: {e:?}"))?;
        if let Some(e) = crate::sema::check_program(&prog).into_iter().next() {
            return Err(format!("sema: {}", e.message));
        }
        let (layouts, lerrs) = crate::layout::compute(&prog);
        if let Some(e) = lerrs.into_iter().next() {
            return Err(format!("layout: {}", e.message));
        }
        let ir = crate::lower::lower(&prog, &layouts).map_err(|e| format!("lower: {e}"))?;
        let errs = crate::ir::verify(&ir);
        assert!(errs.is_empty(), "ir::verify: {errs:?}");
        Ok(ir)
    }

    /// Lower and run `fname(args)`, returning its `I64` result.
    fn run_i64(src: &str, fname: &str, args: &[i64]) -> Result<i64, String> {
        let ir = lower_program(src)?;
        let interp = IrInterp::new(&ir);
        let rargs: Vec<RVal> = args.iter().map(|&i| RVal::Int(i)).collect();
        match interp.run(fname, &rargs).map_err(|e| format!("run: {e}"))? {
            Some(v) => Ok(v.as_i64()),
            None => Ok(0),
        }
    }

    /// Differential gate: the IR program output must equal the AST oracle's output.
    fn assert_matches_oracle(src: &str) {
        let prog = crate::parser::parse(src).expect("parse");
        let oracle = crate::interp::run_to_string(&prog).expect("oracle run");
        let ir = lower_program(src).expect("lower");
        let got = IrInterp::new(&ir).run_program().expect("ir run");
        assert_eq!(got, oracle, "IR output != oracle for source:\n{src}");
    }

    // ---- scalar / control-flow / call core ----

    #[test]
    fn recursive_fib() {
        let src = "I64 T(I64 n) { if (n < 2) return n; return T(n - 1) + T(n - 2); }";
        assert_eq!(run_i64(src, "T", &[10]).unwrap(), 55);
    }

    #[test]
    fn while_loop_sum() {
        let src =
            "I64 T(I64 n) { I64 s = 0; I64 i = 1; while (i <= n) { s += i; i++; } return s; }";
        assert_eq!(run_i64(src, "T", &[100]).unwrap(), 5050);
    }

    #[test]
    fn narrow_int_truncates_at_return() {
        assert_eq!(run_i64("U8 T() { return 300; }", "T", &[]).unwrap(), 44);
    }

    // ---- flat-memory core ----

    #[test]
    fn pointer_to_local() {
        let src = "I64 T() { I64 x = 5; I64 *p = &x; *p = 9; return x; }";
        assert_eq!(run_i64(src, "T", &[]).unwrap(), 9);
    }

    #[test]
    fn local_array_sum() {
        let src = "I64 T() { I64 a[3]; a[0]=10; a[1]=20; a[2]=30; I64 s=0; for(I64 i=0;i<3;i++) s+=a[i]; return s; }";
        assert_eq!(run_i64(src, "T", &[]).unwrap(), 60);
    }

    #[test]
    fn struct_member_and_copy() {
        let src = "
            class P { I64 x; I64 y; }
            I64 T() { P a; a.x=7; a.y=8; P b; b=a; b.x=100; return a.x + b.x + b.y; }";
        assert_eq!(run_i64(src, "T", &[]).unwrap(), 115);
    }

    #[test]
    fn pointer_arithmetic_walks_array() {
        let src =
            "I64 T() { I64 a[5]={1,2,3,4,5}; I64 *p=&a[0]; I64 *q=p+4; return *q - *p + (q - p); }";
        assert_eq!(run_i64(src, "T", &[]).unwrap(), 8);
    }

    // ---- heap ----

    #[test]
    fn heap_alloc_store_load() {
        let src = "I64 T() { I64 *p = MAlloc(8 * 3); p[0]=11; p[1]=22; p[2]=33; I64 s=p[0]+p[1]+p[2]; Free(p); return s; }";
        assert_eq!(run_i64(src, "T", &[]).unwrap(), 66);
    }

    #[test]
    fn distinct_allocations_are_disjoint() {
        let src = "
            I64 T() {
                I64 *a = MAlloc(8);
                I64 *b = MAlloc(8);
                *a = 1; *b = 2;
                return *a * 10 + *b;
            }";
        assert_eq!(run_i64(src, "T", &[]).unwrap(), 12);
    }

    // ---- differential gate against the AST oracle (output) ----

    #[test]
    fn bare_string_print_matches_oracle() {
        assert_matches_oracle("\"Hello, world!\\n\";");
    }

    #[test]
    fn multiple_bare_strings_match_oracle() {
        assert_matches_oracle("\"one\\n\"; \"two\\n\"; \"three\\n\";");
    }

    #[test]
    fn direct_stdwrite_matches_oracle() {
        assert_matches_oracle("#include <unistd.hc>\nStdWrite(1, \"hi there\\n\", 9);");
    }

    #[test]
    fn print_inside_loop_matches_oracle() {
        // A top-level loop emitting a bare string each iteration.
        assert_matches_oracle("I64 i; for (i = 0; i < 3; i++) \"x\\n\";");
    }

    // ---- the printf path (varargs → Print → VFmt → StdWrite) ----

    #[test]
    fn printf_single_int_matches_oracle() {
        assert_matches_oracle("\"n = %d\\n\", 42;");
    }

    #[test]
    fn printf_multiple_ints_matches_oracle() {
        assert_matches_oracle("\"%d + %d = %d\\n\", 2, 3, 2 + 3;");
    }

    #[test]
    fn printf_string_and_char_matches_oracle() {
        assert_matches_oracle("\"%s is %c\\n\", \"grade\", 'A';");
    }

    #[test]
    fn printf_hex_and_unsigned_matches_oracle() {
        assert_matches_oracle("\"%x %u %d\\n\", 255, 7, -7;");
    }

    #[test]
    fn printf_in_a_loop_matches_oracle() {
        assert_matches_oracle("I64 i; for (i = 1; i <= 5; i++) \"i=%d\\n\", i;");
    }

    #[test]
    fn printf_explicit_print_call_matches_oracle() {
        assert_matches_oracle("Print(\"%d items\\n\", 3);");
    }

    #[test]
    fn printf_width_and_padding_matches_oracle() {
        assert_matches_oracle("\"[%5d][%-5d][%05d]\\n\", 42, 42, 42;");
    }

    #[test]
    fn printf_float_matches_oracle() {
        // The hardest path: FmtFloat over the Fbn base-2^32 bignum.
        assert_matches_oracle("\"%f\\n\", 3.14159;");
    }

    #[test]
    fn printf_float_g_matches_oracle() {
        assert_matches_oracle("\"%g %g\\n\", 0.001, 123456.0;");
    }

    // ---- switch ----

    #[test]
    fn switch_cases_break_default_match_oracle() {
        assert_matches_oracle(
            "I64 i; for (i = 0; i < 5; i++) switch (i) { \
             case 0: \"zero\\n\"; break; \
             case 1: case 2: \"one-or-two\\n\"; break; \
             default: \"other %d\\n\", i; }",
        );
    }

    #[test]
    fn switch_range_and_fallthrough_match_oracle() {
        assert_matches_oracle(
            "I64 i; for (i = 0; i < 6; i++) switch (i) { \
             case 1 ... 3: \"mid %d\\n\", i; default: \"reached %d\\n\", i; }",
        );
    }

    #[test]
    fn switch_start_end_sublabels_match_oracle() {
        assert_matches_oracle(
            "I64 i; for (i = 0; i < 3; i++) switch (i) { \
             start: \"[\"; case 1: \"one\"; end: \"]\\n\"; }",
        );
    }

    // ---- indirect calls & aggregate-by-value ----

    #[test]
    fn indirect_call_via_func_pointer_matches_oracle() {
        assert_matches_oracle(
            "I64 Add(I64 a, I64 b) { return a + b; } \
             I64 Apply(I64 (*f)(I64, I64), I64 x, I64 y) { return f(x, y); } \
             \"sum=%d\\n\", Apply(&Add, 3, 4);",
        );
    }

    #[test]
    fn struct_by_value_return_and_arg_match_oracle() {
        assert_matches_oracle(
            "class P { I64 x; I64 y; } \
             P Mk(I64 a, I64 b) { P p; p.x = a; p.y = b; return p; } \
             I64 Sum(P p) { return p.x + p.y; } \
             \"%d\\n\", Sum(Mk(3, 4));",
        );
    }

    #[test]
    fn tuple_return_and_index_match_oracle() {
        assert_matches_oracle(
            "(I64, I64) DivMod(I64 a, I64 b) { return (a / b, a % b); } \
             (I64, I64) t = DivMod(17, 5); \"%d %d\\n\", t[0], t[1];",
        );
    }

    // ---- exceptions ----

    #[test]
    fn try_catch_value_matches_oracle() {
        assert_matches_oracle(
            "try { throw(42); \"unreached\\n\"; } catch { \"caught %d\\n\", Fs->except_ch; }",
        );
    }

    #[test]
    fn throw_across_a_call_matches_oracle() {
        assert_matches_oracle(
            "U0 Deep(I64 n) { if (n > 3) throw(n); \"  n=%d\\n\", n; } \
             I64 i; try { for (i = 0; i < 9; i++) Deep(i); } \
             catch { \"stopped at %d\\n\", Fs->except_ch; }",
        );
    }

    #[test]
    fn nested_try_rethrow_and_flag_match_oracle() {
        assert_matches_oracle(
            "try { try { throw(7); } catch { \"inner %d\\n\", Fs->except_ch; throw; } } \
             catch { \"outer %d flag=%d\\n\", Fs->except_ch, Fs->catch_except; } \
             \"flag now %d\\n\", Fs->catch_except;",
        );
    }
}
