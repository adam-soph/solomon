//! OS-primitive instruction selection for the arm64 backend ([`crate::backend::arm64::isel::FnEmit`]): the heap,
//! clock, thread, atomic/futex, file, and process primitives `emit_prim` dispatches to.

use crate::backend::arm64::isel::*;

impl crate::backend::arm64::isel::FnEmit<'_> {
    pub(super) fn emit_prim(
        &mut self,
        dst: Option<Vreg>,
        prim: Prim,
        args: &[Val],
        width: Option<IrTy>,
    ) -> Result<(), CodegenError> {
        // Win32 functions (`<windows.hc>`) are Windows-only; this is the AArch64
        // backend. They are gated behind `#ifdef _WIN32`, so a real compile for an
        // AArch64 target never reaches here — this is a clean diagnostic for misuse.
        if let Prim::WinCall { func } = prim {
            return Err(self.unsupported(&format!("`{func}` requires the Windows target")));
        }
        // `Open` needs the per-OS flag/ABI handling.
        if let Prim::Open = prim {
            return self.emit_open(dst, args);
        }
        // Atomics (width-directed), the memory fence, and the futex.
        if matches!(
            prim,
            Prim::AtomicLoad
                | Prim::AtomicStore
                | Prim::AtomicAdd
                | Prim::AtomicSwap
                | Prim::AtomicCas
        ) {
            return self.emit_atomic(dst, prim, args, width.unwrap_or(IrTy::I64));
        }
        if let Prim::AtomicFence = prim {
            self.asm.dmb_ish();
            return Ok(());
        }
        if matches!(prim, Prim::FutexWait | Prim::FutexWake) {
            return self.emit_futex(prim, args);
        }
        if matches!(prim, Prim::Thread | Prim::Join) {
            return self.emit_thread(dst, prim, args);
        }
        // The clock primitives read a per-clock-id timespec; `Sleep` builds one.
        if matches!(prim, Prim::UnixNS | Prim::NanoNS | Prim::CpuNS) {
            self.emit_clock(dst, prim);
            return Ok(());
        }
        if let Prim::Sleep = prim {
            self.emit_sleep(args)?;
            return Ok(());
        }
        // The heap primitives: freestanding calls the `mmap` runtime routines; Darwin
        // maps them to libc (`HeapExtend` has no libc equivalent, so it returns NULL and
        // `ReAlloc` falls back to allocate-copy-free).
        if matches!(
            prim,
            Prim::MAlloc | Prim::Free | Prim::HeapExtend | Prim::MSize
        ) {
            return self.emit_heap_prim(dst, prim, args);
        }
        // Identity/process and filesystem-mutation ops branch on the target internally.
        match prim {
            Prim::Getpid | Prim::Getppid | Prim::Getuid | Prim::Getgid => {
                return self.emit_procid(dst, prim);
            }
            Prim::Remove | Prim::Rename | Prim::Mkdir | Prim::Chdir => {
                return self.emit_fsop(dst, prim, args);
            }
            Prim::Getcwd => return self.emit_getcwd(dst, args),
            _ => {}
        }
        if self.ctx.freestanding {
            return self.emit_syscall_prim(dst, prim, args);
        }
        // Hosted Darwin: the remaining supported primitives map to libc calls. The ones
        // returning a C `int` (`close`/`socket`/`connect`) are sign-extended to I64.
        let (sym, sext) = match prim {
            Prim::StdWrite | Prim::Write => ("_write", false),
            Prim::Read => ("_read", false),
            Prim::LSeek => ("_lseek", false),
            Prim::Close => ("_close", true),
            Prim::Socket => ("_socket", true),
            Prim::Connect => ("_connect", true),
            Prim::Exit => ("_exit", false),
            other => {
                return Err(self.unsupported(&format!("primitive {other:?}")));
            }
        };
        self.place_prim_args(args)?;
        self.asm.bl_extern(sym);
        if let Some(d) = dst {
            if sext {
                self.asm.mov_reg(TMP0, 0);
                self.emit_int_cast(IrTy::I32);
                self.store_vreg(d, TMP0);
            } else {
                self.store_vreg(d, 0);
            }
        }
        Ok(())
    }

    /// `Getpid`/`Getppid`/`Getuid`/`Getgid` → the id in x0. Freestanding: a bare syscall;
    /// Darwin: libc, with the `int`/`uint` result extended to I64 (every real id is a
    /// small non-negative value, so the extend signedness is immaterial).
    fn emit_procid(&mut self, dst: Option<Vreg>, prim: Prim) -> Result<(), CodegenError> {
        if self.ctx.freestanding {
            let nr: i64 = match prim {
                Prim::Getpid => 172,
                Prim::Getppid => 173,
                Prim::Getuid => 174,
                _ => 176, // Getgid
            };
            self.asm.load_imm(SCRATCH, nr);
            self.asm.svc();
        } else {
            let sym = match prim {
                Prim::Getpid => "_getpid",
                Prim::Getppid => "_getppid",
                Prim::Getuid => "_getuid",
                _ => "_getgid",
            };
            self.asm.bl_extern(sym);
        }
        if let Some(d) = dst {
            self.asm.mov_reg(TMP0, 0);
            let to = if matches!(prim, Prim::Getuid | Prim::Getgid) {
                IrTy::U32
            } else {
                IrTy::I32
            };
            self.emit_int_cast(to);
            self.store_vreg(d, TMP0);
        }
        Ok(())
    }

    /// `Remove`/`Rename`/`Mkdir`/`Chdir` → 0 or `-errno`. Freestanding uses the `*at`
    /// syscalls (no bare `unlink`/`rename`/`mkdir`) with an `AT_FDCWD` prepend; `chdir`
    /// is bare. Darwin calls libc and converts the `-1`/errno failure to `-errno`.
    fn emit_fsop(
        &mut self,
        dst: Option<Vreg>,
        prim: Prim,
        args: &[Val],
    ) -> Result<(), CodegenError> {
        if self.ctx.freestanding {
            match prim {
                Prim::Remove => {
                    self.load_val(args[0], 1); // x1 = path
                    self.asm.load_imm(0, -100); // x0 = AT_FDCWD
                    self.asm.load_imm(2, 0); // x2 = flags
                    self.asm.load_imm(SCRATCH, 35); // SYS_unlinkat
                }
                Prim::Chdir => {
                    self.load_val(args[0], 0); // x0 = path
                    self.asm.load_imm(SCRATCH, 49); // SYS_chdir
                }
                Prim::Rename => {
                    self.load_val(args[0], 1); // x1 = oldpath
                    self.load_val(args[1], 3); // x3 = newpath
                    self.asm.load_imm(0, -100); // x0 = AT_FDCWD (old)
                    self.asm.load_imm(2, -100); // x2 = AT_FDCWD (new)
                    self.asm.load_imm(SCRATCH, 38); // SYS_renameat
                }
                Prim::Mkdir => {
                    self.load_val(args[0], 1); // x1 = path
                    self.load_val(args[1], 2); // x2 = mode
                    self.asm.load_imm(0, -100); // x0 = AT_FDCWD
                    self.asm.load_imm(SCRATCH, 34); // SYS_mkdirat
                }
                _ => unreachable!(),
            }
            self.asm.svc();
            if let Some(d) = dst {
                self.store_vreg(d, 0); // 0 / -errno
            }
            return Ok(());
        }
        let sym = match prim {
            Prim::Remove => "_unlink",
            Prim::Rename => "_rename",
            Prim::Mkdir => "_mkdir",
            Prim::Chdir => "_chdir",
            _ => unreachable!(),
        };
        self.place_prim_args(args)?; // path[, newpath/mode] in x0[, x1]
        self.asm.bl_extern(sym);
        self.asm.mov_reg(TMP0, 0);
        self.emit_int_cast(IrTy::I32); // sign-extend the libc `int`
        self.emit_errno_neg(); // -1 → -errno (normalised)
        if let Some(d) = dst {
            self.store_vreg(d, TMP0);
        }
        Ok(())
    }

    /// `Getcwd(buf, size)` → 0 or `-errno`. Freestanding `getcwd` returns the byte length
    /// on success (normalised to 0); Darwin libc returns `buf` (non-NULL → 0) or NULL
    /// (→ `-errno`).
    fn emit_getcwd(&mut self, dst: Option<Vreg>, args: &[Val]) -> Result<(), CodegenError> {
        self.place_prim_args(args)?; // x0 = buf, x1 = size
        if self.ctx.freestanding {
            self.asm.load_imm(SCRATCH, 17); // SYS_getcwd
            self.asm.svc();
            self.asm.mov_reg(TMP0, 0);
            // A non-negative length becomes 0; a negative -errno passes through.
            let neg = self.asm.new_label();
            self.asm.cmp_imm(TMP0, 0);
            self.asm.b_cond(C_LT, neg);
            self.asm.load_imm(TMP0, 0);
            self.asm.place(neg);
        } else {
            self.asm.bl_extern("_getcwd");
            self.asm.mov_reg(TMP0, 0);
            let done = self.asm.new_label();
            let fail = self.asm.new_label();
            self.asm.cmp_imm(TMP0, 0);
            self.asm.b_cond(C_EQ, fail);
            self.asm.load_imm(TMP0, 0); // non-NULL → 0
            self.asm.b(done);
            self.asm.place(fail);
            self.asm.bl_extern("___error");
            self.asm.ldr_w(0, 0); // w0 = errno
            self.asm.neg(TMP0, 0); // -errno
            self.asm.place(done);
        }
        if let Some(d) = dst {
            self.store_vreg(d, TMP0);
        }
        Ok(())
    }

    /// Load up to 8 primitive arguments into x0.. (in order).
    fn place_prim_args(&mut self, args: &[Val]) -> Result<(), CodegenError> {
        for (i, a) in args.iter().enumerate() {
            if i > 7 {
                return Err(self.unsupported("more than 8 primitive arguments"));
            }
            self.load_val(*a, i as u32);
        }
        Ok(())
    }

    /// The heap primitives. Freestanding: call the `mmap` bump-allocator runtime routine
    /// (`MAlloc`/`Free`/`HeapExtend`/`MSize`) via its label. Darwin: `MAlloc`→`_malloc`,
    /// `Free`→`_free`, `MSize`→0 (unsupported), `HeapExtend`→NULL.
    fn emit_heap_prim(
        &mut self,
        dst: Option<Vreg>,
        prim: Prim,
        args: &[Val],
    ) -> Result<(), CodegenError> {
        if self.ctx.freestanding {
            let name = match prim {
                Prim::MAlloc => "MAlloc",
                Prim::Free => "Free",
                Prim::HeapExtend => "HeapExtend",
                Prim::MSize => "MSize",
                _ => unreachable!(),
            };
            let label = *self
                .ctx
                .heap_labels
                .get(name)
                .ok_or_else(|| self.unsupported("heap routine not emitted"))?;
            self.place_prim_args(args)?;
            self.asm.bl(label);
            if let Some(d) = dst {
                self.store_vreg(d, 0);
            }
            return Ok(());
        }
        // Darwin.
        match prim {
            Prim::HeapExtend => {
                if let Some(d) = dst {
                    self.asm.load_imm(0, 0); // no in-place grow on hosted
                    self.store_vreg(d, 0);
                }
            }
            Prim::Free => {
                self.place_prim_args(args)?;
                self.asm.bl_extern("_free");
            }
            Prim::MAlloc => {
                self.place_prim_args(args)?;
                self.asm.bl_extern("_malloc");
                if let Some(d) = dst {
                    self.store_vreg(d, 0);
                }
            }
            Prim::MSize => {
                if let Some(d) = dst {
                    self.asm.load_imm(0, 0);
                    self.store_vreg(d, 0);
                }
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    /// A clock primitive (`UnixNS`/`NanoNS`/`CpuNS`) → nanoseconds since its epoch. Reads
    /// a `timespec` on the stack via `clock_gettime` (the libc call on Darwin, the syscall
    /// freestanding) and folds it to `tv_sec * 1e9 + tv_nsec`. The clock id differs per OS
    /// (REALTIME 0/0, MONOTONIC 1/6, PROCESS_CPUTIME 2/12).
    fn emit_clock(&mut self, dst: Option<Vreg>, prim: Prim) {
        let (linux_id, macos_id): (i64, i64) = match prim {
            Prim::UnixNS => (0, 0),
            Prim::NanoNS => (1, 6),
            Prim::CpuNS => (2, 12),
            _ => unreachable!(),
        };
        self.asm.sub_sp_imm(16); // a 16-byte stack timespec (keeps 16-alignment)
        self.asm.add_imm(1, SP, 0); // x1 = &ts
        if self.ctx.freestanding {
            self.asm.load_imm(0, linux_id);
            self.asm.load_imm(SCRATCH, 113); // SYS_clock_gettime
            self.asm.svc();
        } else {
            self.asm.load_imm(0, macos_id);
            self.asm.bl_extern("_clock_gettime");
        }
        self.asm.load_mem(TMP1, SP, 8, false); // tv_sec  @ +0
        self.asm.ldur(TMP2, SP, 8); // tv_nsec @ +8
        self.asm.add_sp_imm(16);
        self.asm.load_imm(ADDR, 1_000_000_000);
        self.asm.mul(TMP1, TMP1, ADDR);
        self.asm.add(TMP0, TMP1, TMP2); // TMP0 = sec*1e9 + nsec
        if let Some(d) = dst {
            self.store_vreg(d, TMP0);
        }
    }

    /// `Sleep(ns)`: build a `timespec` (`ns/1e9`, `ns%1e9`) and `nanosleep`.
    fn emit_sleep(&mut self, args: &[Val]) -> Result<(), CodegenError> {
        if args.len() != 1 {
            return Err(self.unsupported("Sleep with other than 1 argument"));
        }
        self.load_val(args[0], TMP0); // ns
        self.asm.load_imm(ADDR, 1_000_000_000);
        self.asm.udiv(TMP1, TMP0, ADDR); // tv_sec
        self.asm.msub(TMP2, TMP1, ADDR, TMP0); // tv_nsec = ns - sec*1e9
        self.asm.sub_sp_imm(16);
        self.asm.store_mem(TMP1, SP, 8); // tv_sec  @ +0
        self.asm.stur(TMP2, SP, 8); // tv_nsec @ +8
        self.asm.add_imm(0, SP, 0); // x0 = &ts
        self.asm.load_imm(1, 0); // rem = NULL
        if self.ctx.freestanding {
            self.asm.load_imm(SCRATCH, 101); // SYS_nanosleep
            self.asm.svc();
        } else {
            self.asm.bl_extern("_nanosleep");
        }
        self.asm.add_sp_imm(16);
        Ok(())
    }

    /// `Thread(&fn, arg)` spawns a thread running `fn(arg)`; `Join(handle)` waits for it
    /// and returns its result. Hosted Darwin uses `pthread_create`/`pthread_join`;
    /// freestanding aarch64-linux uses raw `clone(2)` + a futex join (see
    /// [`Self::emit_thread_fs`]).
    ///
    /// NB: `Fs` is single-task on both arm64 targets (one shared `CTask`), so a program
    /// that throws inside **concurrently-running** threads would race on the shared
    /// exception state. Non-exception parallelism (atomics / futex locks) is correct.
    fn emit_thread(
        &mut self,
        dst: Option<Vreg>,
        prim: Prim,
        args: &[Val],
    ) -> Result<(), CodegenError> {
        if self.ctx.freestanding {
            return match prim {
                Prim::Thread => self.emit_thread_fs(dst, args),
                Prim::Join => self.emit_join_fs(dst, args),
                _ => unreachable!(),
            };
        }
        self.asm.sub_sp_imm(16); // a stack slot for the tid / retval out-param
        match prim {
            Prim::Thread => {
                self.load_val(args[1], 3); // x3 = arg
                self.load_val(args[0], 2); // x2 = start routine (function address)
                self.asm.add_imm(0, SP, 0); // x0 = &tid
                self.asm.load_imm(1, 0); // x1 = NULL attr
                self.asm.bl_extern("_pthread_create");
                self.asm.load_mem(TMP0, SP, 8, false); // TMP0 = tid (the handle)
            }
            Prim::Join => {
                self.load_val(args[0], 0); // x0 = handle
                self.asm.add_imm(1, SP, 0); // x1 = &retval
                self.asm.bl_extern("_pthread_join");
                self.asm.load_mem(TMP0, SP, 8, false); // TMP0 = the thread's return value
            }
            _ => unreachable!(),
        }
        self.asm.add_sp_imm(16);
        if let Some(d) = dst {
            self.store_vreg(d, TMP0);
        }
        Ok(())
    }

    /// Freestanding `Thread(&fn, arg)`: spawn a `CLONE_THREAD` thread via `clone(2)` onto
    /// an `mmap`'d 128 KiB region, running `fn(arg)`. A 32-byte control block at the
    /// region base — `[retval | ctid futex | fn | arg]` — carries `fn`/`arg` in and the
    /// result back; its address is the handle. `CLONE_PARENT_SETTID` writes the tid into
    /// the `ctid` futex word *synchronously* (so `Join` can't race a not-yet-set word) and
    /// `CLONE_CHILD_CLEARTID` zeroes it + futex-wakes on exit — how `Join` waits.
    ///
    /// Register-allocation-safe: nothing rides into the child in a callee-saved register
    /// (which register promotion may have claimed). The child instead recovers the base
    /// from its own `sp` (`base = sp - (STACK_SIZE - 16)`, the stack top it was cloned
    /// with), recomputing it after the call clobbers the scratch register; the parent
    /// keeps the base on its own stack across the `clone` syscall.
    fn emit_thread_fs(&mut self, dst: Option<Vreg>, args: &[Val]) -> Result<(), CodegenError> {
        const STACK_SIZE: i64 = crate::backend::THREAD_STACK_SIZE as i64; // 128 KiB stack + control block
        // CLONE_VM|FS|FILES|SIGHAND|THREAD|PARENT_SETTID|CHILD_CLEARTID.
        const FLAGS: i64 = 0x31_0F00;

        // mmap(0, SIZE, PROT_READ|WRITE, MAP_PRIVATE|ANON, -1, 0) -> x0 = base.
        self.asm.load_imm(0, 0);
        self.asm.load_imm(1, STACK_SIZE);
        self.asm.load_imm(2, 3); // PROT_READ|PROT_WRITE
        self.asm.load_imm(3, 0x22); // MAP_PRIVATE|MAP_ANONYMOUS
        self.asm.load_imm(4, -1);
        self.asm.load_imm(5, 0);
        self.asm.load_imm(SCRATCH, 222); // SYS_mmap
        self.asm.svc();
        self.asm.mov_reg(ADDR, 0); // ADDR = base

        // Keep the base on the parent stack across the clone syscall (the handle).
        self.asm.sub_sp_imm(16);
        self.asm.str_sp(ADDR, 0); // [sp] = base
        // Control block: [base+16] = fn, [base+24] = arg.
        self.load_val(args[0], TMP0);
        self.asm.store_mem_off(TMP0, ADDR, 16, 8);
        self.load_val(args[1], TMP0);
        self.asm.store_mem_off(TMP0, ADDR, 24, 8);

        // clone(flags, child_sp, ptid=&futex, tls=0, ctid=&futex).
        let l_child = self.asm.new_label();
        let l_done = self.asm.new_label();
        self.asm.load_imm(0, FLAGS);
        self.asm.load_imm(SCRATCH, STACK_SIZE - 16);
        self.asm.add(1, ADDR, SCRATCH); // x1 = child stack top
        self.asm.add_imm(2, ADDR, 8); // x2 = ptid = &futex (set synchronously)
        self.asm.load_imm(3, 0); // x3 = tls
        self.asm.add_imm(4, ADDR, 8); // x4 = ctid = &futex (cleared + woken on exit)
        self.asm.load_imm(SCRATCH, 220); // SYS_clone
        self.asm.svc();
        self.asm.cbz(0, l_child);

        // Parent: the handle is the base. Reclaim the stack slot and finish.
        self.asm.load_mem_off(TMP0, SP, 0, 8, false);
        self.asm.add_sp_imm(16);
        self.asm.b(l_done);

        // Child: recover base from sp, run fn(arg), stash the result, exit (which fires
        // CLONE_CHILD_CLEARTID, waking a joiner).
        self.asm.place(l_child);
        self.asm.add_imm(TMP1, SP, 0); // TMP1 = sp = child stack top
        self.asm.load_imm(SCRATCH, STACK_SIZE - 16);
        self.asm.sub(TMP1, TMP1, SCRATCH); // TMP1 = base
        self.asm.load_mem_off(TMP0, TMP1, 16, 8, false); // fn
        self.asm.load_mem_off(0, TMP1, 24, 8, false); // x0 = arg
        self.asm.blr(TMP0); // x0 = fn(arg); the call clobbers TMP1
        self.asm.add_imm(TMP1, SP, 0); // recompute base from the (restored) sp
        self.asm.load_imm(SCRATCH, STACK_SIZE - 16);
        self.asm.sub(TMP1, TMP1, SCRATCH);
        self.asm.store_mem_off(0, TMP1, 0, 8); // [base+0] = retval
        self.asm.load_imm(0, 0);
        self.asm.load_imm(SCRATCH, 93); // SYS_exit (this thread)
        self.asm.svc();

        self.asm.place(l_done);
        if let Some(d) = dst {
            self.store_vreg(d, TMP0); // parent: handle = base
        }
        Ok(())
    }

    /// Freestanding `Join(handle)`: futex-wait on the control block's `ctid` word until
    /// the kernel clears it (thread exit), then return the `retval` the thread left. The
    /// base is held on the stack across the futex syscall so no callee-saved register
    /// (possibly promoted) is needed.
    fn emit_join_fs(&mut self, dst: Option<Vreg>, args: &[Val]) -> Result<(), CodegenError> {
        self.load_val(args[0], TMP1); // base (handle)
        self.asm.sub_sp_imm(16);
        self.asm.str_sp(TMP1, 0); // [sp] = base

        let l_wait = self.asm.new_label();
        let l_done = self.asm.new_label();
        self.asm.place(l_wait);
        self.asm.load_mem_off(TMP1, SP, 0, 8, false); // base
        self.asm.load_mem_off(TMP0, TMP1, 8, 8, false); // *ctid (0 once the thread exits)
        self.asm.cbz(TMP0, l_done);
        // futex(&ctid, FUTEX_WAIT=0, val=*ctid, timeout=NULL).
        self.asm.add_imm(0, TMP1, 8); // uaddr
        self.asm.load_imm(1, 0); // FUTEX_WAIT
        self.asm.mov_reg(2, TMP0); // val = the tid we observed
        self.asm.load_imm(3, 0); // timeout = NULL
        self.asm.load_imm(SCRATCH, 98); // SYS_futex
        self.asm.svc();
        self.asm.b(l_wait);

        self.asm.place(l_done);
        self.asm.load_mem_off(TMP1, SP, 0, 8, false); // base
        self.asm.load_mem_off(TMP0, TMP1, 0, 8, false); // [base+0] = retval
        self.asm.add_sp_imm(16);
        if let Some(d) = dst {
            self.store_vreg(d, TMP0);
        }
        Ok(())
    }

    /// An atomic op (`stdatomic.hc`), width-directed by `width` (the pointee type).
    /// Load/store use `ldar`/`stlr`; add/swap/cas use `ldaxr`/`stlxr` retry loops. The
    /// witnessed/result value is sign/zero-extended to the pointee width so it matches a
    /// normal load.
    fn emit_atomic(
        &mut self,
        dst: Option<Vreg>,
        prim: Prim,
        args: &[Val],
        width: IrTy,
    ) -> Result<(), CodegenError> {
        let sz = match width.size() {
            1 => 0,
            2 => 1,
            4 => 2,
            _ => 3,
        };
        match prim {
            Prim::AtomicLoad => {
                self.load_val(args[0], ADDR);
                self.asm.ldar(TMP0, ADDR, sz);
                self.emit_int_cast(width);
            }
            Prim::AtomicStore => {
                self.load_val(args[0], ADDR);
                self.load_val(args[1], TMP0);
                self.asm.stlr(TMP0, ADDR, sz);
            }
            Prim::AtomicAdd => {
                self.load_val(args[0], ADDR);
                self.load_val(args[1], TMP2); // delta
                let l = self.asm.new_label();
                self.asm.place(l);
                self.asm.ldaxr(TMP0, ADDR, sz); // old
                self.emit_int_cast(width); // extend old (correct add for a narrow type)
                self.asm.add(TMP0, TMP0, TMP2); // new = old + delta
                self.asm.stlxr(TMP1, TMP0, ADDR, sz);
                self.asm.cbnz(TMP1, l);
                self.emit_int_cast(width); // extend the stored-width result
            }
            Prim::AtomicSwap => {
                self.load_val(args[0], ADDR);
                self.load_val(args[1], TMP2); // new value
                let l = self.asm.new_label();
                self.asm.place(l);
                self.asm.ldaxr(TMP0, ADDR, sz); // old (the result)
                self.asm.stlxr(TMP1, TMP2, ADDR, sz);
                self.asm.cbnz(TMP1, l);
                self.emit_int_cast(width);
            }
            Prim::AtomicCas => {
                self.load_val(args[0], ADDR);
                self.load_val(args[1], TMP2); // expected
                self.load_val(args[2], SCRATCH); // desired
                let l = self.asm.new_label();
                let done = self.asm.new_label();
                self.asm.place(l);
                self.asm.ldaxr(TMP0, ADDR, sz); // old (witnessed)
                self.emit_int_cast(width);
                self.asm.cmp_reg(TMP0, TMP2);
                self.asm.b_cond(C_NE, done); // mismatch → return old, no store
                self.asm.stlxr(TMP1, SCRATCH, ADDR, sz);
                self.asm.cbnz(TMP1, l); // lost the monitor → retry
                self.asm.place(done);
            }
            _ => unreachable!(),
        }
        if let Some(d) = dst {
            self.store_vreg(d, TMP0);
        }
        Ok(())
    }

    /// `FutexWait(addr, val)` / `FutexWake(addr, n)`. Freestanding uses the Linux
    /// `futex(2)` syscall; Darwin uses libc `__ulock_wait`/`__ulock_wake`. A `FutexWait`
    /// carries a short timeout so a missed wakeup degrades to a periodic re-check.
    fn emit_futex(&mut self, prim: Prim, args: &[Val]) -> Result<(), CodegenError> {
        let wake = matches!(prim, Prim::FutexWake);
        const FUTEX_TIMEOUT_NS: i64 = crate::backend::FUTEX_TIMEOUT_NS as i64;
        if self.ctx.freestanding {
            self.load_val(args[0], 0); // x0 = uaddr
            self.load_val(args[1], 2); // x2 = val (expected / n)
            self.asm.load_imm(1, if wake { 1 } else { 0 }); // FUTEX_WAKE / FUTEX_WAIT
            if wake {
                self.asm.load_imm(3, 0); // no timeout
            } else {
                self.asm.sub_sp_imm(16); // relative timespec {0, TIMEOUT} on the stack
                self.asm.load_imm(TMP0, 0);
                self.asm.str_sp(TMP0, 0); // tv_sec
                self.asm.load_imm(TMP0, FUTEX_TIMEOUT_NS);
                self.asm.str_sp(TMP0, 8); // tv_nsec
                self.asm.add_imm(3, SP, 0); // x3 = &timespec
            }
            self.asm.load_imm(4, 0); // uaddr2
            self.asm.load_imm(5, 0); // val3
            self.asm.load_imm(SCRATCH, 98); // SYS_futex
            self.asm.svc();
            if !wake {
                self.asm.add_sp_imm(16);
            }
        } else {
            self.load_val(args[0], 1); // x1 = addr
            self.load_val(args[1], 2); // x2 = value (ignored for wake)
            if wake {
                self.asm.load_imm(2, 0); // wake one
                self.asm.load_imm(3, 0);
            } else {
                self.asm.load_imm(3, FUTEX_TIMEOUT_NS / 1000); // timeout µs
            }
            self.asm.load_imm(0, 1); // UL_COMPARE_AND_WAIT
            self.asm.bl_extern(if wake {
                "___ulock_wake"
            } else {
                "___ulock_wait"
            });
        }
        Ok(())
    }

    /// Freestanding primitives backed by raw `aarch64` Linux syscalls (args in x0.., the
    /// number in x8, `svc`; the kernel returns the result or `-errno` in x0).
    fn emit_syscall_prim(
        &mut self,
        dst: Option<Vreg>,
        prim: Prim,
        args: &[Val],
    ) -> Result<(), CodegenError> {
        let nr: i64 = match prim {
            Prim::StdWrite | Prim::Write => 64, // write
            Prim::Read => 63,                   // read
            Prim::Close => 57,                  // close
            Prim::LSeek => 62,                  // lseek
            Prim::Socket => 198,                // socket
            Prim::Connect => 203,               // connect
            Prim::Exit => 94,                   // exit_group
            other => {
                return Err(self.unsupported(&format!("freestanding primitive {other:?}")));
            }
        };
        self.place_prim_args(args)?;
        self.asm.load_imm(SCRATCH, nr); // x8 = syscall number
        self.asm.svc();
        if let Some(d) = dst {
            self.store_vreg(d, 0);
        }
        Ok(())
    }

    /// `Open(path, flags, mode)`. Freestanding: `openat(AT_FDCWD, path, flags, mode)` —
    /// aarch64 has no bare `open`, the `fcntl.hc` flags are already Linux's, and the
    /// syscall returns `-errno` directly. Hosted Darwin: translate the Linux-canonical
    /// `O_*` flags to macOS, call the variadic libc `open` (the `mode` arg travels on the
    /// stack), sign-extend the `int` result, and convert a `-1` failure into the
    /// `-errno` (Linux-normalised) the rest of the stdlib returns.
    fn emit_open(&mut self, dst: Option<Vreg>, args: &[Val]) -> Result<(), CodegenError> {
        if args.len() != 3 {
            return Err(self.unsupported("Open with other than 3 arguments"));
        }
        if self.ctx.freestanding {
            self.load_val(args[2], 3); // x3 = mode
            self.load_val(args[1], 2); // x2 = flags (Linux values, verbatim)
            self.load_val(args[0], 1); // x1 = path
            self.asm.load_imm(0, -100); // x0 = AT_FDCWD
            self.asm.load_imm(SCRATCH, 56); // x8 = SYS_openat
            self.asm.svc();
            if let Some(d) = dst {
                self.store_vreg(d, 0); // fd / -errno
            }
            return Ok(());
        }
        self.load_val(args[0], 0); // x0 = path
        self.load_val(args[1], 1); // x1 = flags (Linux values)
        // macos = (f & 3) | (O_CREAT 0x40→0x200) | (O_TRUNC 0x200→0x400) |
        //         (O_APPEND 0x400→0x8): move each `from`-bit to its `to`-bit.
        self.asm.and_imm_lowbits(TMP2, 1, 2); // access mode (low 2 bits)
        for (from, to) in [(6u32, 9u32), (9, 10), (10, 3)] {
            self.asm.lsr_imm(TMP0, 1, from);
            self.asm.and_imm_lowbits(TMP0, TMP0, 1);
            self.asm.lsl_imm(TMP0, TMP0, to);
            self.asm.orr(TMP2, TMP2, TMP0);
        }
        self.asm.mov_reg(1, TMP2); // x1 = translated flags
        self.load_val(args[2], SCRATCH); // mode (the first stack vararg)
        self.asm.sub_sp_imm(16);
        self.asm.str_sp(SCRATCH, 0); // [sp] = mode
        self.asm.bl_extern("_open");
        self.asm.add_sp_imm(16);
        self.asm.mov_reg(TMP0, 0); // result in TMP0
        self.emit_int_cast(IrTy::I32); // sign-extend the libc `int`
        self.emit_errno_neg(); // -1 → -errno (normalised)
        if let Some(d) = dst {
            self.store_vreg(d, TMP0);
        }
        Ok(())
    }

    /// After a libc call whose `int` result is in `TMP0`, convert a `-1` failure to the
    /// `-errno` the freestanding syscalls return: `if (TMP0 < 0) TMP0 = -*___error();`,
    /// with the Darwin errno normalised to its Linux-canonical value (the same table the
    /// interpreter uses, so they can't drift).
    fn emit_errno_neg(&mut self) {
        let ok = self.asm.new_label();
        self.asm.cmp_imm(TMP0, 0);
        self.asm.b_cond(C_GE, ok);
        self.asm.bl_extern("___error");
        self.asm.ldr_w(0, 0); // w0 = errno (Darwin numbering)
        let done = self.asm.new_label();
        for &(darwin, linux) in crate::intrinsics::DARWIN_TO_LINUX_ERRNO {
            let next = self.asm.new_label();
            self.asm.cmp_imm(0, darwin as u32);
            self.asm.b_cond(C_NE, next);
            self.asm.load_imm(0, linux);
            self.asm.b(done);
            self.asm.place(next);
        }
        self.asm.place(done);
        self.asm.neg(TMP0, 0); // TMP0 = -errno
        self.asm.place(ok);
    }
}
