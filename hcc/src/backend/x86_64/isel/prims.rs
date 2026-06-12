//! OS-primitive instruction selection for the x86-64 backend ([`crate::backend::x86_64::isel::FnEmit`]): the
//! heap, thread, atomic/futex, file, and process primitives `emit_prim` dispatches to,
//! plus the Win32 `emit_win_call` import marshalling.

use crate::backend::x86_64::isel::*;

impl crate::backend::x86_64::isel::FnEmit<'_> {
    pub(super) fn emit_prim(
        &mut self,
        dst: Option<Vreg>,
        prim: Prim,
        args: &[Val],
        width: Option<Ty>,
    ) -> Result<(), CodegenError> {
        match prim {
            Prim::Free => {} // a no-op bump allocator never frees
            Prim::MAlloc => return self.emit_heap_call(dst, "MAlloc", args),
            Prim::HeapExtend => return self.emit_heap_call(dst, "HeapExtend", args),
            Prim::MSize => return self.emit_heap_call(dst, "MSize", args),
            Prim::StdWrite => {
                self.load_int_args(args);
                self.os.emit_std_write(self.asm);
                self.store_dst(dst);
            }
            Prim::Open => {
                self.load_int_args(args);
                self.os.emit_fileop(self.asm, FileOp::Open);
                self.store_dst(dst);
            }
            Prim::Read | Prim::Write | Prim::Close | Prim::LSeek => {
                let op = match prim {
                    Prim::Read => FileOp::Read,
                    Prim::Write => FileOp::Write,
                    Prim::Close => FileOp::Close,
                    _ => FileOp::LSeek,
                };
                self.load_int_args(args);
                self.os.emit_fileop(self.asm, op);
                self.store_dst(dst);
            }
            Prim::UnixNS | Prim::NanoNS | Prim::CpuNS => {
                let scratch = self.ctx.clock_scratch.expect("clock scratch");
                match prim {
                    Prim::NanoNS => self.os.emit_mono_ns(self.asm, scratch),
                    Prim::CpuNS => self.os.emit_cpu_ns(self.asm, scratch),
                    _ => self.os.emit_unix_ns(self.asm, scratch),
                }
                self.store_dst(dst);
            }
            Prim::Sleep => {
                self.load_val(args[0], RAX); // rax = ns
                let scratch = self.ctx.clock_scratch.expect("clock scratch");
                self.os.emit_sleep(self.asm, scratch);
            }
            Prim::Exit => {
                self.load_val(args[0], RAX);
                self.os.emit_exit(self.asm);
            }
            Prim::Socket
            | Prim::Connect
            | Prim::Remove
            | Prim::Rename
            | Prim::Mkdir
            | Prim::Getpid
            | Prim::Getppid
            | Prim::Getuid
            | Prim::Getgid
            | Prim::Chdir => {
                self.require_posix(prim)?;
                let nr: i32 = match prim {
                    Prim::Getpid => 39,
                    Prim::Chdir => 80,
                    Prim::Rename => 82,
                    Prim::Mkdir => 83,
                    Prim::Remove => 87,
                    Prim::Getuid => 102,
                    Prim::Getgid => 104,
                    Prim::Getppid => 110,
                    Prim::Socket => 41,
                    Prim::Connect => 42,
                    _ => unreachable!(),
                };
                self.load_int_args(args);
                self.asm.mov_ri(RAX, nr);
                self.asm.syscall();
                self.store_dst(dst);
            }
            Prim::Getcwd => {
                self.require_posix(prim)?;
                self.load_int_args(args); // rdi = buf, rsi = size
                self.asm.mov_ri(RAX, 79);
                self.asm.syscall();
                let neg = self.asm.new_label();
                self.asm.cmp_ri(RAX, 0);
                self.asm.js(neg);
                self.asm.xor_rr(RAX, RAX); // a length → 0
                self.asm.place(neg);
                self.store_dst(dst);
            }
            Prim::Thread => {
                self.require_posix(prim)?;
                self.emit_thread_fs(dst, args);
            }
            Prim::Join => {
                self.require_posix(prim)?;
                self.emit_join_fs(dst, args);
            }
            Prim::System => {
                self.require_posix(prim)?;
                self.emit_system_fs(dst, args);
            }
            Prim::ThreadYield => {
                // Linux sched_yield; Windows kernel32 SwitchToThread. Always 0.
                if let Some(slot) = self.os.extern_slot("SwitchToThread") {
                    self.emit_win_call(slot, &[]);
                } else {
                    self.asm.mov_ri(RAX, 24); // SYS_sched_yield
                    self.asm.syscall();
                }
                self.asm.mov_ri(RAX, 0);
                self.store_dst(dst);
            }
            Prim::ThreadDetach => {
                // Freestanding clone(2) threads have nothing to release (their
                // stacks are never reclaimed, documented in threads.hc); the handle
                // simply must not be joined again. Windows has no threads here yet.
                self.require_posix(prim)?;
                self.asm.mov_ri(RAX, 0);
                self.store_dst(dst);
            }
            Prim::Gettid => {
                // Linux gettid; Windows kernel32 GetCurrentThreadId.
                if let Some(slot) = self.os.extern_slot("GetCurrentThreadId") {
                    self.emit_win_call(slot, &[]);
                } else {
                    self.asm.mov_ri(RAX, 186); // SYS_gettid
                    self.asm.syscall();
                }
                self.store_dst(dst);
            }
            Prim::FutexWaitNs => {
                self.require_posix(prim)?;
                self.emit_futex_wait_ns(args);
                self.store_dst(dst);
            }
            Prim::ThreadExit => {
                self.require_posix(prim)?;
                self.emit_thread_exit(args);
            }
            Prim::AtomicLoad
            | Prim::AtomicStore
            | Prim::AtomicAdd
            | Prim::AtomicSwap
            | Prim::AtomicCas => {
                self.emit_atomic(dst, prim, args, width.unwrap_or(Ty::I64));
            }
            Prim::AtomicFence => self.asm.mfence(),
            Prim::FutexWait | Prim::FutexWake => {
                self.require_posix(prim)?;
                self.emit_futex(prim, args);
            }
            Prim::WinCall { func } => {
                // A Win32 function from `<windows.hc>`: a direct kernel32 import on
                // Windows, rejected elsewhere. `extern_slot` is `Some` only on the
                // Windows target (it has dynamic imports).
                let Some(slot) = self.os.extern_slot(func) else {
                    return Err(self.unsupported(&format!("`{func}` requires the Windows target")));
                };
                self.emit_win_call(slot, args);
                self.store_dst(dst);
            }
        }
        Ok(())
    }

    /// Lower a `Prim::WinCall` to a direct kernel32 import call under the MS x64 ABI:
    /// the HolyC call's int/pointer args go into rcx/rdx/r8/r9 (args 5+ onto the stack
    /// above the 32-byte shadow), rsp is 16-aligned, and the result is left in rax.
    /// Reached only on Windows. Each arg is loaded through rax into its MS home, which
    /// is safe because no arg ever lives in rax/rcx/rdx/r8/r9 (the promotion pool is
    /// callee-saved-only) and slot loads are rbp-relative (immune to the rsp dance).
    fn emit_win_call(&mut self, slot: usize, args: &[Val]) {
        const MS: [u8; 4] = [RCX, RDX, R8, R9];
        let n = args.len();
        let overflow = n.saturating_sub(4) as i32;
        // 32-byte shadow + one 8-byte slot per stack arg, rounded to a 16-multiple so
        // rsp stays 16-aligned at the call (CreateFileA's 7 args → 64, like emit_open).
        let frame = align16(32 + 8 * overflow);
        self.asm.mov_rr(R15, RSP); // save caller rsp (non-volatile; survives the call)
        self.asm.and_ri(RSP, -16); // 16-align
        self.asm.sub_rsp(frame);
        // Stack args (the 5th onward) at [rsp+32], [rsp+40], …
        for k in 0..overflow {
            self.load_val(args[(4 + k) as usize], RAX);
            self.asm.store_rsp_reg((32 + 8 * k) as i8, RAX);
        }
        // Register args → rcx/rdx/r8/r9, loaded through rax then moved into the MS home
        // (mov_rr is REX-safe for r8/r9; a direct slot-load into a high reg would not be).
        for (i, home) in MS.iter().enumerate().take(n) {
            self.load_val(args[i], RAX);
            self.asm.mov_rr(*home, RAX);
        }
        self.asm.call_extern(slot);
        self.asm.mov_rr(RSP, R15); // restore rsp
    }

    fn require_posix(&self, prim: Prim) -> Result<(), CodegenError> {
        if self.os.is_posix() {
            Ok(())
        } else {
            Err(self.unsupported(&format!(
                "`{prim:?}` is not supported on the Windows target yet"
            )))
        }
    }

    /// Load up to three primitive arguments into the syscall/ABI registers rdi/rsi/rdx
    /// (all low registers, so the operands' slots load directly).
    fn load_int_args(&mut self, args: &[Val]) {
        for (i, &a) in args.iter().enumerate().take(3) {
            self.load_val(a, ARG_GPR[i]);
        }
    }

    fn store_dst(&mut self, dst: Option<Vreg>) {
        if let Some(d) = dst {
            self.store_vreg(d, RAX);
        }
    }

    /// Call a heap runtime routine (`MAlloc`/`HeapExtend`/`MSize`) with its args in
    /// rdi/rsi/rdx; the result is in rax.
    fn emit_heap_call(
        &mut self,
        dst: Option<Vreg>,
        routine: &str,
        args: &[Val],
    ) -> Result<(), CodegenError> {
        self.load_int_args(args);
        let label = *self
            .ctx
            .heap_labels
            .get(routine)
            .ok_or_else(|| self.unsupported("heap routine not emitted"))?;
        self.asm.call(label);
        self.store_dst(dst);
        Ok(())
    }

    /// An atomic op (`stdatomic.hc`), width-directed by `width` (the pointee type). On
    /// x86-64 a plain aligned `mov` is an atomic acquire load / release store; add/swap/cas
    /// use the `lock`-prefixed `xadd`/`xchg`/`cmpxchg`. The result is sign/zero-extended.
    fn emit_atomic(&mut self, dst: Option<Vreg>, prim: Prim, args: &[Val], width: Ty) {
        let w = width.size() as i32;
        let signed = width.is_signed();
        match prim {
            Prim::AtomicLoad => {
                self.load_val(args[0], RAX); // rax = p
                self.asm.load_through(w, signed); // rax = [p]
            }
            Prim::AtomicStore => {
                self.load_val(args[0], RCX); // rcx = p (store_through writes [rcx])
                self.load_val(args[1], RAX); // rax = v
                self.asm.store_through(w);
            }
            Prim::AtomicAdd => {
                self.load_val(args[0], RSI); // rsi = p
                self.load_val(args[1], RAX); // rax = delta
                self.asm.mov_rr(RDX, RAX); // rdx = delta (kept past the xadd)
                self.asm.lock_xadd(RSI, RAX, w); // rax = old, [rsi] += delta
                self.asm.cast_rax(w, signed); // extend old
                self.asm.add_rr(RAX, RDX); // new = old + delta
                self.asm.cast_rax(w, signed);
            }
            Prim::AtomicSwap => {
                self.load_val(args[0], RSI); // rsi = p
                self.load_val(args[1], RAX); // rax = v
                self.asm.xchg_mem(RSI, RAX, w); // rax = old, [rsi] = v
                self.asm.cast_rax(w, signed);
            }
            Prim::AtomicCas => {
                self.load_val(args[0], RSI); // rsi = p
                self.load_val(args[1], RAX); // rax = expected (the cmpxchg comparand)
                self.load_val(args[2], RCX); // rcx = desired
                self.asm.lock_cmpxchg(RSI, RCX, w); // if [rsi]==acc then [rsi]=rcx; acc=old
                self.asm.cast_rax(w, signed);
            }
            _ => unreachable!(),
        }
        self.store_dst(dst);
    }

    /// `FutexWait(addr, val)` / `FutexWake(addr, n)` via the Linux `futex(2)` syscall
    /// (`FUTEX_WAIT` 0 / `FUTEX_WAKE` 1) on the low 32 bits of `*addr`. A `FutexWait`
    /// carries a short relative timeout, so a missed wakeup re-checks rather than deadlocks.
    fn emit_futex(&mut self, prim: Prim, args: &[Val]) {
        const FUTEX_TIMEOUT_NS: i32 = crate::backend::FUTEX_TIMEOUT_NS as i32; // ≈1 ms
        let wake = matches!(prim, Prim::FutexWake);
        self.load_val(args[0], RDI); // rdi = uaddr
        self.load_val(args[1], RDX); // rdx = val (expected / n)
        self.asm.mov_ri(RSI, if wake { 1 } else { 0 }); // FUTEX_WAKE / FUTEX_WAIT
        if wake {
            self.asm.mov_ri(R10, 0); // no timeout
        } else {
            // Relative `struct timespec {0, FUTEX_TIMEOUT_NS}` on the stack -> r10.
            self.asm.add_ri(RSP, -16);
            self.asm.store_rsp_imm(0, 0); // tv_sec
            self.asm.store_rsp_imm(8, FUTEX_TIMEOUT_NS); // tv_nsec
            self.asm.mov_rr(R10, RSP);
        }
        self.asm.mov_ri(R8, 0); // uaddr2
        self.asm.mov_ri(R9, 0); // val3
        self.asm.mov_ri(RAX, 202); // SYS_futex
        self.asm.syscall();
        if !wake {
            self.asm.add_ri(RSP, 16);
        }
    }

    /// `FutexWaitNs(addr, expected, ns)`: `FutexWait` with the caller's timeout in
    /// place of the internal ~1 ms one. Leaves the kernel's result in rax (0 = woken,
    /// -ETIMEDOUT / -EAGAIN otherwise); the caller stores it.
    fn emit_futex_wait_ns(&mut self, args: &[Val]) {
        // timespec { ns/1e9, ns%1e9 } on the stack.
        self.load_val(args[2], RAX); // ns
        self.asm.mov_ri(RCX, 1_000_000_000);
        self.asm.div_rcx(); // rax = sec, rdx = nsec
        self.asm.sub_rsp(16);
        self.asm.store_rsp_reg(0, RAX); // tv_sec
        self.asm.store_rsp_reg(8, RDX); // tv_nsec
        self.load_val(args[0], RDI); // uaddr
        self.load_val(args[1], RDX); // expected
        self.asm.mov_ri(RSI, 0); // FUTEX_WAIT
        self.asm.mov_rr(R10, RSP); // &timespec (relative)
        self.asm.mov_ri(R8, 0); // uaddr2
        self.asm.mov_ri(R9, 0); // val3
        self.asm.mov_ri(RAX, 202); // SYS_futex
        self.asm.syscall();
        self.asm.add_ri(RSP, 16);
    }

    /// Freestanding `Thread(fn, arg)`: spawn a `CLONE_THREAD` thread via `clone(2)` onto an
    /// `mmap`'d stack running `fn(arg)`. A 32-byte control block at the stack base —
    /// `[retval | ctid futex | fn | arg]` — carries the closure in and the result back; its
    /// address is the handle. `base` rides into the child in callee-saved rbx (saved/restored
    /// around the spawn on the parent path).
    fn emit_thread_fs(&mut self, dst: Option<Vreg>, args: &[Val]) {
        const SIZE: i32 = crate::backend::THREAD_STACK_SIZE as i32; // 128 KiB stack + control block
        // CLONE_VM|FS|FILES|SIGHAND|THREAD|SYSVSEM|SETTLS|PARENT_SETTID|CHILD_CLEARTID.
        const FLAGS: i32 = 0x3D_0F00;
        const TLS_OFF: i32 = 0x40; // a TLS self-pointer slot past the 32-byte block

        self.asm.push_reg(RBX); // save the caller's rbx
        // mmap(0, SIZE, PROT_READ|WRITE, MAP_PRIVATE|ANON, -1, 0) -> rax = base.
        self.asm.mov_ri(RDI, 0);
        self.asm.mov_ri(RSI, SIZE);
        self.asm.mov_ri(RDX, 3);
        self.asm.mov_ri(R10, 0x22);
        self.asm.mov_ri(R8, -1);
        self.asm.mov_ri(R9, 0);
        self.asm.mov_ri(RAX, 9); // mmap
        self.asm.syscall();
        self.asm.mov_rr(RBX, RAX); // rbx = base (survives the syscall, inherited by child)
        // control block: [base+16] = fn, [base+24] = arg.
        self.load_val(args[0], RAX);
        self.asm.mov_rr(RCX, RBX);
        self.asm.add_ri(RCX, 16);
        self.asm.store_qword_at(RCX, RAX);
        self.load_val(args[1], RAX);
        self.asm.mov_rr(RCX, RBX);
        self.asm.add_ri(RCX, 24);
        self.asm.store_qword_at(RCX, RAX);
        // TLS self-pointer: [base+TLS_OFF] = base+TLS_OFF.
        self.asm.mov_rr(RCX, RBX);
        self.asm.add_ri(RCX, TLS_OFF);
        self.asm.store_qword_at(RCX, RCX);

        let l_child = self.asm.new_label();
        let l_done = self.asm.new_label();
        // clone(FLAGS, child_sp, ptid=&futex, ctid=&futex, tls=&TLS).
        self.asm.mov_rr(RSI, RBX);
        self.asm.add_ri(RSI, SIZE - 16); // child stack top
        self.asm.mov_rr(RDX, RBX);
        self.asm.add_ri(RDX, 8); // ptid = &futex
        self.asm.mov_rr(R10, RBX);
        self.asm.add_ri(R10, 8); // ctid = &futex
        self.asm.mov_rr(R8, RBX);
        self.asm.add_ri(R8, TLS_OFF); // tls = &TLS
        self.asm.mov_ri(RDI, FLAGS);
        self.asm.mov_ri(RAX, 56); // clone
        self.asm.syscall();
        self.asm.test_rax();
        self.asm.je(l_child);
        // Parent: rbx still holds base (the handle). Restore rbx and finish.
        self.asm.mov_rr(RAX, RBX);
        self.asm.pop_reg(RBX);
        self.asm.jmp(l_done);
        // Child: rax == 0, rbx = base. Run fn(arg), stash the result, exit.
        self.asm.place(l_child);
        self.asm.mov_rr(RDX, RBX);
        self.asm.add_ri(RDX, 24);
        self.asm.load_qword_at(RDI, RDX); // rdi = arg
        self.asm.mov_rr(RDX, RBX);
        self.asm.add_ri(RDX, 16);
        self.asm.load_qword_at(RAX, RDX); // rax = fn
        self.asm.call_reg(RAX); // fn(arg); rbx survives (callee-saved)
        self.asm.store_qword_at(RBX, RAX); // [base+0] = return
        self.asm.mov_ri(RDI, 0);
        self.asm.mov_ri(RAX, 60); // exit (this thread; fires CLONE_CHILD_CLEARTID)
        self.asm.syscall();
        self.asm.place(l_done);
        self.store_dst(dst);
    }

    /// Freestanding `Join(handle)`: futex-wait on the control block's `ctid` word until the
    /// kernel clears it (thread exit), then return the stashed `retval`. `base` is held in
    /// callee-saved rbx across the syscall (saved/restored).
    fn emit_join_fs(&mut self, dst: Option<Vreg>, args: &[Val]) {
        self.asm.push_reg(RBX);
        self.load_val(args[0], RAX);
        self.asm.mov_rr(RBX, RAX); // rbx = base (handle)
        let l_wait = self.asm.new_label();
        let l_done = self.asm.new_label();
        self.asm.place(l_wait);
        self.asm.mov_rr(RCX, RBX);
        self.asm.add_ri(RCX, 8);
        self.asm.load_qword_at(RAX, RCX); // rax = *futex (0 once the thread exits)
        self.asm.test_rax();
        self.asm.je(l_done);
        self.asm.mov_rr(RDX, RAX); // val = observed tid
        self.asm.mov_rr(RDI, RBX);
        self.asm.add_ri(RDI, 8); // uaddr = &futex
        self.asm.mov_ri(RSI, 0); // FUTEX_WAIT
        self.asm.mov_ri(R10, 0); // timeout = NULL
        self.asm.mov_ri(RAX, 202); // SYS_futex
        self.asm.syscall();
        self.asm.jmp(l_wait);
        self.asm.place(l_done);
        self.asm.mov_rr(RAX, RBX);
        self.asm.load_qword_at(RAX, RAX); // rax = [base+0] = retval
        self.asm.pop_reg(RBX);
        self.store_dst(dst);
    }

    /// `ThreadExit(ret)`: end the calling thread with `ret` as its `Join` value; from
    /// the main flow, end the program with status `ret`. The thread case stores `ret`
    /// into the clone control block, found via the `%fs` base `CLONE_SETTLS` seeded
    /// (the self-pointer slot sits at `base + 0x40`); the base is read with
    /// `arch_prctl(ARCH_GET_FS)`, which is 0 on the main flow — `%fs:[0]` can't be
    /// dereferenced there.
    fn emit_thread_exit(&mut self, args: &[Val]) {
        const TLS_OFF: i32 = 0x40; // matches emit_thread_fs
        let l_thread = self.asm.new_label();
        // arch_prctl(ARCH_GET_FS = 0x1003, &out) -> rcx = the fs base.
        self.asm.sub_rsp(16);
        self.asm.mov_ri(RDI, 0x1003);
        self.asm.mov_rr(RSI, RSP);
        self.asm.mov_ri(RAX, 158); // SYS_arch_prctl
        self.asm.syscall();
        self.asm.mov_rr(RCX, RSP);
        self.asm.load_qword_at(RCX, RCX);
        self.asm.add_ri(RSP, 16);
        self.load_val(args[0], RDI); // ret
        self.asm.test_rr(RCX, RCX);
        self.asm.jne(l_thread);
        self.asm.mov_ri(RAX, 231); // main flow: exit_group(ret)
        self.asm.syscall();
        self.asm.place(l_thread);
        self.asm.add_ri(RCX, -TLS_OFF); // base = fsbase - TLS_OFF
        self.asm.store_qword_at(RCX, RDI); // [base+0] = retval, as the child path does
        self.asm.mov_ri(RDI, 0);
        self.asm.mov_ri(RAX, 60); // SYS_exit (this thread; fires CHILD_CLEARTID)
        self.asm.syscall();
    }

    /// Freestanding `System(cmd)`: `fork`, `execve("/bin/sh", ["sh", "-c", cmd],
    /// NULL)` in the child (exit 127 if the exec fails, like sh), `wait4` in the
    /// parent. Returns the child's exit code (0–255), -1 on an abnormal (signalled)
    /// exit, or the syscall's -errno when the spawn/wait fails. A 48-byte stack frame
    /// holds the strings (64-bit immediates) and the argv array — the child is a full
    /// fork copy, so its COW view of the frame stays valid:
    /// `[sh\0 | -c\0 | argv0 | argv1 | argv2=cmd | NULL]`.
    fn emit_system_fs(&mut self, dst: Option<Vreg>, args: &[Val]) {
        const SH: u64 = 0x0068_732F_6E69_622F; // "/bin/sh\0", little-endian
        const DASH_C: u64 = 0x0000_0000_0000_632D; // "-c\0"
        let l_child = self.asm.new_label();
        let l_sig = self.asm.new_label();
        let l_fin = self.asm.new_label();

        self.asm.sub_rsp(48);
        self.asm.mov_ri64(RCX, SH);
        self.asm.store_rsp_reg(0, RCX);
        self.asm.mov_ri64(RCX, DASH_C);
        self.asm.store_rsp_reg(8, RCX);
        self.asm.mov_rr(RCX, RSP);
        self.asm.store_rsp_reg(16, RCX); // argv[0] = rsp
        self.asm.add_ri(RCX, 8);
        self.asm.store_rsp_reg(24, RCX); // argv[1] = rsp+8
        self.load_val(args[0], RAX); // cmd (vreg slots are rbp-relative)
        self.asm.store_rsp_reg(32, RAX); // argv[2] = cmd
        self.asm.mov_ri(RCX, 0);
        self.asm.store_rsp_reg(40, RCX); // argv[3] = NULL

        self.asm.mov_ri(RAX, 57); // fork
        self.asm.syscall();
        self.asm.test_rax();
        self.asm.je(l_child);
        // Parent: rax = pid, or the fork's -errno (returned as-is).
        self.asm.js(l_fin);
        // wait4(pid, &status, 0, 0); zero the status slot first (the kernel writes
        // only 4 bytes and the slot still holds "/bin/sh").
        self.asm.mov_ri(RCX, 0);
        self.asm.store_rsp_reg(0, RCX);
        self.asm.mov_rr(RDI, RAX); // pid
        self.asm.mov_rr(RSI, RSP); // &status
        self.asm.mov_ri(RDX, 0);
        self.asm.mov_ri(R10, 0);
        self.asm.mov_ri(RAX, 61); // wait4
        self.asm.syscall();
        self.asm.test_rax();
        self.asm.js(l_fin); // wait4 failed: its -errno
        // Decode the status word: signalled → -1, else (st >> 8) & 0xff.
        self.asm.mov_rr(RCX, RSP);
        self.asm.load_qword_at(RAX, RCX);
        self.asm.mov_rr(RCX, RAX);
        self.asm.and_ri(RCX, 0x7f);
        self.asm.test_rr(RCX, RCX);
        self.asm.jne(l_sig);
        self.asm.shr_ri(RAX, 8);
        self.asm.and_ri(RAX, 0xff);
        self.asm.jmp(l_fin);
        self.asm.place(l_sig);
        self.asm.mov_ri(RAX, -1);
        self.asm.jmp(l_fin);

        // Child: execve("/bin/sh", argv, NULL); exit_group(127) if the exec fails.
        self.asm.place(l_child);
        self.asm.mov_rr(RDI, RSP); // path
        self.asm.mov_rr(RSI, RSP);
        self.asm.add_ri(RSI, 16); // argv
        self.asm.mov_ri(RDX, 0); // envp = NULL
        self.asm.mov_ri(RAX, 59); // execve
        self.asm.syscall();
        self.asm.mov_ri(RDI, 127);
        self.asm.mov_ri(RAX, 231); // exit_group
        self.asm.syscall();

        self.asm.place(l_fin);
        self.asm.add_ri(RSP, 48);
        self.store_dst(dst);
    }
}
