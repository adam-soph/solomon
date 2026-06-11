//! Exception lowering for the x86-64 backend ([`crate::backend::x86_64::isel::FnEmit`]): the jmp_buf/longjmp-style
//! `try`/`throw` unwinder over the `Fs->exc_top` handler-frame chain.

use crate::backend::x86_64::isel::*;

impl crate::backend::x86_64::isel::FnEmit<'_> {
    // ---- exceptions (jmp_buf/longjmp-style unwind over Fs->exc_top) ----
    //
    // The `ExcFrame` (32 bytes) is `{ prev, saved_rsp, saved_rbp, landing_pad }`. Spill-all
    // keeps nothing in callee-saved registers, so no callee-saved set is saved.

    /// Load the current task pointer (`*Fs`, a `CTask *`) into `reg`.
    fn fs_ptr(&mut self, reg: u8) {
        let fs = self.ctx.fs_gid.expect("Fs accessed without the Fs global");
        self.asm.lea_global(reg, self.ctx.global_bss[fs as usize]);
        self.asm.load_qword_at(reg, reg); // reg = *(&Fs) = CTask*
    }

    /// `TryBegin`: build the on-stack `ExcFrame` and push it onto `Fs->exc_top`. The frame
    /// is the alloca `frame`; its fields are at `[rbp - slot_off ..]`.
    pub(super) fn emit_try_begin(&mut self, pad: BlockId, frame: SlotId) {
        let exc_top = self.ctx.exc_top_off;
        // frame.prev = Fs->exc_top
        self.fs_ptr(ADDR); // rsi = CTask*
        self.asm.mov_rr(RAX, ADDR);
        self.asm.add_ri(RAX, exc_top);
        self.asm.load_through(8, false); // rax = Fs->exc_top
        self.slot_addr(frame, 0, RCX); // rcx = &ExcFrame
        self.asm.store_qword_at(RCX, RAX); // frame.prev
        // frame.saved_rsp / saved_rbp.
        self.slot_addr(frame, 8, RCX);
        self.asm.store_qword_at(RCX, RSP);
        self.slot_addr(frame, 16, RCX);
        self.asm.store_qword_at(RCX, RBP);
        // frame.landing_pad = &pad
        self.asm.lea_rax_label(self.block_labels[pad as usize]);
        self.slot_addr(frame, 24, RCX);
        self.asm.store_qword_at(RCX, RAX);
        // Fs->exc_top = &ExcFrame
        self.fs_ptr(RCX);
        self.asm.add_ri(RCX, exc_top);
        self.slot_addr(frame, 0, RAX);
        self.asm.store_qword_at(RCX, RAX);
    }

    /// `TryEnd`: normal completion pops the handler (`Fs->exc_top = Fs->exc_top->prev`).
    pub(super) fn emit_try_end(&mut self) {
        let exc_top = self.ctx.exc_top_off;
        self.fs_ptr(ADDR); // rsi = CTask*
        self.asm.mov_rr(RCX, ADDR);
        self.asm.add_ri(RCX, exc_top); // rcx = &exc_top
        self.asm.load_qword_at(RAX, RCX); // rax = top
        self.asm.load_qword_at(RAX, RAX); // rax = top->prev (offset 0)
        self.asm.store_qword_at(RCX, RAX);
    }

    /// `Throw`/`Rethrow`: unwind to the nearest handler — restore its rsp/rbp from the top
    /// `ExcFrame`, pop it, and jump to its landing pad. An empty chain exits with the thrown
    /// value (`Fs->except_ch`).
    pub(super) fn emit_unwind(&mut self) {
        let exc_top = self.ctx.exc_top_off;
        self.fs_ptr(ADDR); // rsi = CTask*
        self.asm.mov_rr(RCX, ADDR);
        self.asm.add_ri(RCX, exc_top);
        self.asm.load_qword_at(R11, RCX); // r11 = top frame
        let live = self.asm.new_label();
        self.asm.test_rr(R11, R11);
        self.asm.jne(live);
        // Uncaught: exit(Fs->except_ch).
        self.asm.mov_rr(RAX, ADDR);
        self.asm.add_ri(RAX, self.ctx.except_ch_off);
        self.asm.load_through(8, false);
        self.os.emit_exit(self.asm);
        self.asm.place(live);
        // Fs->exc_top = top->prev.
        self.asm.load_qword_at(RAX, R11); // rax = top->prev
        self.fs_ptr(RCX);
        self.asm.add_ri(RCX, exc_top);
        self.asm.store_qword_at(RCX, RAX);
        // Restore rsp then rbp from the frame, then jump to its landing pad.
        self.asm.mov_rr(RAX, R11);
        self.asm.add_ri(RAX, 8);
        self.asm.load_through(8, false);
        self.asm.mov_rr(RSP, RAX); // saved_rsp
        self.asm.mov_rr(RAX, R11);
        self.asm.add_ri(RAX, 16);
        self.asm.load_through(8, false);
        self.asm.mov_rr(RBP, RAX); // saved_rbp
        self.asm.mov_rr(RAX, R11);
        self.asm.add_ri(RAX, 24);
        self.asm.load_through(8, false); // landing_pad
        self.asm.jmp_reg(RAX);
    }
}
