//! Exception lowering for the arm64 backend ([`crate::backend::arm64::isel::FnEmit`]): the jmp_buf/longjmp-style
//! `try`/`throw` unwinder over the `Fs->exc_top` handler-frame chain.

use crate::backend::arm64::isel::*;

impl crate::backend::arm64::isel::FnEmit<'_> {
    // ---- exceptions (jmp_buf/longjmp-style unwind over `Fs->exc_top`) ----
    //
    // An `ExcFrame` (32 bytes) is `{ prev, saved_sp, saved_fp, landing_pad }`. A
    // `try`-containing function is left fully spilled by `plan_registers`, so no
    // callee-saved set is saved here. Scratch regs TMP0/TMP1/TMP2 are caller-saved and
    // reloaded per instruction, so they are free to clobber.

    /// Load the current task pointer (`*Fs`, a `CTask *`) into `reg`.
    fn fs_ptr(&mut self, reg: u32) {
        let fs = self
            .ctx
            .fs_gid
            .expect("Fs accessed in a program without the Fs global");
        self.global_addr_into(reg, fs, 0);
        self.asm.load_mem(reg, reg, 8, false); // reg = *(&Fs) = CTask*
    }

    /// `TryBegin`: build the on-stack `ExcFrame` and push it onto `Fs->exc_top`.
    pub(super) fn emit_try_begin(&mut self, pad: BlockId, frame: SlotId) {
        let exc_top = self.ctx.exc_top_off;
        self.slot_addr(frame, 0, TMP1); // TMP1 = &ExcFrame
        self.fs_ptr(TMP2); // TMP2 = CTask*
        // frame.prev = Fs->exc_top
        self.asm.load_mem_off(TMP0, TMP2, exc_top, 8, false);
        self.asm.store_mem_off(TMP0, TMP1, 0, 8);
        // frame.saved_sp = sp ; frame.saved_fp = x29
        self.asm.add_imm(TMP0, SP, 0);
        self.asm.store_mem_off(TMP0, TMP1, 8, 8);
        self.asm.store_mem_off(FP, TMP1, 16, 8);
        // frame.landing_pad = &pad
        self.asm.adr_label(TMP0, self.block_labels[pad as usize]);
        self.asm.store_mem_off(TMP0, TMP1, 24, 8);
        // Fs->exc_top = &ExcFrame
        self.asm.store_mem_off(TMP1, TMP2, exc_top, 8);
    }

    /// `TryEnd`: normal completion pops the handler (`Fs->exc_top = Fs->exc_top->prev`).
    pub(super) fn emit_try_end(&mut self) {
        let exc_top = self.ctx.exc_top_off;
        self.fs_ptr(TMP2); // CTask*
        self.asm.load_mem_off(TMP0, TMP2, exc_top, 8, false); // top
        self.asm.load_mem_off(TMP1, TMP0, 0, 8, false); // top->prev
        self.asm.store_mem_off(TMP1, TMP2, exc_top, 8);
    }

    /// `Throw`/`Rethrow`: unwind to the nearest handler — restore its sp/fp from the top
    /// `ExcFrame`, pop it, and branch to its landing pad. An empty chain is an uncaught
    /// exception, which exits with the thrown value (`Fs->except_ch`) as the code.
    pub(super) fn emit_unwind(&mut self) {
        let exc_top = self.ctx.exc_top_off;
        self.fs_ptr(TMP2); // TMP2 = CTask*
        self.asm.load_mem_off(TMP1, TMP2, exc_top, 8, false); // TMP1 = top frame
        let live = self.asm.new_label();
        self.asm.cbnz(TMP1, live);
        // Uncaught: exit(Fs->except_ch) (the thrown value the lowering already stored).
        self.asm
            .load_mem_off(0, TMP2, self.ctx.except_ch_off, 8, false);
        if self.ctx.freestanding {
            self.asm.load_imm(SCRATCH, 94); // x8 = SYS_exit_group
            self.asm.svc();
        } else {
            self.asm.bl_extern("_exit");
        }
        self.asm.place(live);
        // Fs->exc_top = top->prev
        self.asm.load_mem_off(TMP0, TMP1, 0, 8, false);
        self.asm.store_mem_off(TMP0, TMP2, exc_top, 8);
        // Restore sp then fp from the frame, then branch to its landing pad.
        self.asm.load_mem_off(TMP0, TMP1, 8, 8, false); // saved_sp
        self.asm.add_imm(SP, TMP0, 0);
        self.asm.load_mem_off(FP, TMP1, 16, 8, false); // saved_fp
        self.asm.load_mem_off(IND, TMP1, 24, 8, false); // landing_pad
        self.asm.br(IND);
    }
}
