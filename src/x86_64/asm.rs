//! The x86-64 instruction emitter — pure ISA, no OS knowledge.
//!
//! `Asm` accumulates machine-code bytes, labels, and fixups, and resolves them
//! in [`Asm::finish`] into a single mapped image laid out `[code | strings |
//! BSS]`. It is deliberately OS-agnostic: the syscall/`kernel32` sequences and
//! the executable container (ELF / PE) live in the per-OS policy modules
//! (`linux`, `windows`), reached through the parent module's `OsTarget` seam.
//! Keeping the encoder here means OS specifics cannot leak into code generation.

use super::{load_opcode, store_opcode};
use crate::codegen::CodegenError;

/// Accumulates raw machine code plus labels and rel32 fixups for jumps/calls.
pub(super) struct Asm {
    code: Vec<u8>,
    labels: Vec<Option<usize>>,
    fixups: Vec<(usize, usize)>, // (rel32 byte position, label)
    strings: Vec<Vec<u8>>,
    str_fixups: Vec<(usize, usize)>, // (RIP-relative disp32 position, string index)
    global_fixups: Vec<(usize, i32)>, // (RIP-relative disp32 position, BSS byte offset)
    extern_calls: Vec<(usize, usize)>, // (RIP-relative disp32 position, extern slot index)
}

impl Asm {
    pub(super) fn new() -> Self {
        Asm {
            code: Vec::new(),
            labels: Vec::new(),
            fixups: Vec::new(),
            strings: Vec::new(),
            str_fixups: Vec::new(),
            global_fixups: Vec::new(),
            extern_calls: Vec::new(),
        }
    }

    /// Current code length, and the total byte length of the interned strings.
    /// A container writer needs these to place an appended region (e.g. a PE
    /// import table) at the right address before [`finish`](Asm::finish) runs.
    pub(super) fn code_len(&self) -> usize {
        self.code.len()
    }
    pub(super) fn strings_total(&self) -> usize {
        self.strings.iter().map(|s| s.len()).sum()
    }

    /// `call qword [rip + disp32]` through external slot `idx` — an indirect call
    /// whose target address lives in a container-supplied table (on Windows, the
    /// import address table). The disp32 is resolved in [`finish`](Asm::finish)
    /// against the `iat_offsets` it is given. Purely an indirect call to an
    /// externally-placed pointer; the encoder knows nothing of imports or DLLs.
    pub(super) fn call_extern(&mut self, idx: usize) {
        self.emit(&[0xFF, 0x15]); // call qword ptr [rip + disp32]
        self.extern_calls.push((self.code.len(), idx));
        self.emit(&[0, 0, 0, 0]);
    }
    pub(super) fn emit(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }
    pub(super) fn new_label(&mut self) -> usize {
        self.labels.push(None);
        self.labels.len() - 1
    }
    pub(super) fn place(&mut self, label: usize) {
        self.labels[label] = Some(self.code.len());
    }
    /// Intern `bytes` as string data (appended after the code); returns its index.
    pub(super) fn intern(&mut self, bytes: &[u8]) -> usize {
        if let Some(i) = self.strings.iter().position(|s| s == bytes) {
            return i;
        }
        self.strings.push(bytes.to_vec());
        self.strings.len() - 1
    }
    /// Resolve all fixups and assemble the final mapped image, laid out
    /// `[code | strings | import | bss]` as one contiguously-mapped blob — so
    /// every RIP-relative reference is just `target - (pos + 4)` regardless of the
    /// load address. `import` (empty on Linux, where there are no imports) is
    /// appended after the strings; `iat_offsets[idx]` is the byte offset, *within*
    /// `import`, of the indirect-call target for [`call_extern`](Asm::call_extern)
    /// slot `idx`. The BSS (`bss` bytes the caller reserves) follows in memory but
    /// not in the file. With `import = &[]` and no extern calls this is identical
    /// to the plain `[code | strings | bss]` ELF layout.
    pub(super) fn finish(
        mut self,
        import: &[u8],
        iat_offsets: &[usize],
    ) -> Result<Vec<u8>, CodegenError> {
        for &(pos, label) in &self.fixups {
            let target = self.labels[label]
                .ok_or_else(|| CodegenError::new("x86_64 backend: unplaced label", None))?;
            let disp = target as i64 - (pos as i64 + 4);
            self.code[pos..pos + 4].copy_from_slice(&(disp as i32).to_le_bytes());
        }
        // Lay the strings out right after the code and patch RIP-relative refs.
        let code_end = self.code.len();
        let mut offsets = Vec::with_capacity(self.strings.len());
        let mut cursor = code_end;
        for s in &self.strings {
            offsets.push(cursor);
            cursor += s.len();
        }
        for &(pos, idx) in &self.str_fixups {
            let disp = offsets[idx] as i64 - (pos as i64 + 4);
            self.code[pos..pos + 4].copy_from_slice(&(disp as i32).to_le_bytes());
        }
        for s in &self.strings {
            self.code.extend_from_slice(s);
        }
        // The import region follows the strings; an extern call resolves to its
        // IAT slot within it (`import_base + iat_offsets[idx]`).
        let import_base = self.code.len();
        for &(pos, idx) in &self.extern_calls {
            let target = import_base + iat_offsets[idx];
            let disp = target as i64 - (pos as i64 + 4);
            self.code[pos..pos + 4].copy_from_slice(&(disp as i32).to_le_bytes());
        }
        self.code.extend_from_slice(import);
        // The BSS region follows everything in the address space (zero-filled, never
        // in the file). A global ref resolves to `bss_base + off`.
        let bss_base = self.code.len();
        for &(pos, off) in &self.global_fixups {
            let disp = (bss_base as i64 + off as i64) - (pos as i64 + 4);
            self.code[pos..pos + 4].copy_from_slice(&(disp as i32).to_le_bytes());
        }
        Ok(self.code)
    }

    /// Emit `opcode` followed by a placeholder rel32 patched to reach `label`.
    pub(super) fn jcc(&mut self, opcode: &[u8], label: usize) {
        self.emit(opcode);
        self.fixups.push((self.code.len(), label));
        self.emit(&[0, 0, 0, 0]);
    }
    pub(super) fn jmp(&mut self, label: usize) {
        self.jcc(&[0xE9], label);
    }
    pub(super) fn je(&mut self, label: usize) {
        self.jcc(&[0x0F, 0x84], label);
    }
    pub(super) fn jne(&mut self, label: usize) {
        self.jcc(&[0x0F, 0x85], label);
    }
    pub(super) fn jns(&mut self, label: usize) {
        self.jcc(&[0x0F, 0x89], label);
    }
    pub(super) fn js(&mut self, label: usize) {
        self.jcc(&[0x0F, 0x88], label);
    }
    pub(super) fn jbe(&mut self, label: usize) {
        self.jcc(&[0x0F, 0x86], label);
    }
    pub(super) fn jb(&mut self, label: usize) {
        self.jcc(&[0x0F, 0x82], label);
    }
    pub(super) fn jae(&mut self, label: usize) {
        self.jcc(&[0x0F, 0x83], label);
    }
    pub(super) fn jp(&mut self, label: usize) {
        self.jcc(&[0x0F, 0x8A], label);
    }
    pub(super) fn jl(&mut self, label: usize) {
        self.jcc(&[0x0F, 0x8C], label);
    }
    pub(super) fn jge(&mut self, label: usize) {
        self.jcc(&[0x0F, 0x8D], label);
    }
    pub(super) fn jg(&mut self, label: usize) {
        self.jcc(&[0x0F, 0x8F], label);
    }
    pub(super) fn jle(&mut self, label: usize) {
        self.jcc(&[0x0F, 0x8E], label);
    }
    pub(super) fn call(&mut self, label: usize) {
        self.jcc(&[0xE8], label);
    }
    /// `call <reg>` — an indirect call through a register (`FF /2`). Used for a
    /// function-pointer call once the target address is in `reg`.
    pub(super) fn call_reg(&mut self, reg: u8) {
        if reg >= 8 {
            self.emit(&[0x41]); // REX.B for r8..r15
        }
        self.emit(&[0xFF, 0xD0 | (reg & 7)]);
    }
    /// `lea rax, [rip + disp32]` to a code `label` — the address of a function (for
    /// `&Func`). Reuses the rel32 code-label fixup (disp is relative to the end of
    /// the displacement, exactly the RIP-relative semantics).
    pub(super) fn lea_rax_label(&mut self, label: usize) {
        self.emit(&[0x48, 0x8D, 0x05]); // lea rax, [rip+disp32]
        self.fixups.push((self.code.len(), label));
        self.emit(&[0, 0, 0, 0]);
    }
    /// `lea rsi, [rip + disp32]` to an interned string (a RIP-relative fixup).
    pub(super) fn lea_rsi_string(&mut self, idx: usize) {
        self.emit(&[0x48, 0x8D, 0x35]); // lea rsi, [rip+disp32]
        self.str_fixups.push((self.code.len(), idx));
        self.emit(&[0, 0, 0, 0]);
    }
    /// `lea rax, [rip + disp32]` to an interned string.
    pub(super) fn lea_rax_string(&mut self, idx: usize) {
        self.emit(&[0x48, 0x8D, 0x05]); // lea rax, [rip+disp32]
        self.str_fixups.push((self.code.len(), idx));
        self.emit(&[0, 0, 0, 0]);
    }
    /// `lea rax, [rip + disp32]` to a global (BSS) at byte offset `off`.
    pub(super) fn lea_rax_global(&mut self, off: i32) {
        self.lea_global(0, off); // 0 = rax
    }
    /// `lea <reg>, [rip + disp32]` to a global (BSS) at byte offset `off`.
    pub(super) fn lea_global(&mut self, reg: u8, off: i32) {
        // REX.W (+ REX.R if reg is an extended register), opcode 8D, ModRM = RIP
        // base (mod 00, rm 101) with the destination in the reg field.
        let rex = 0x48 | if reg >= 8 { 0x04 } else { 0 };
        self.emit(&[rex, 0x8D, 0x05 | ((reg & 7) << 3)]);
        self.global_fixups.push((self.code.len(), off));
        self.emit(&[0, 0, 0, 0]);
    }
    /// `mov rdx, imm32` (the write length).
    pub(super) fn mov_rdx_imm32(&mut self, imm: i32) {
        self.emit(&[0x48, 0xC7, 0xC2]);
        self.emit(&imm.to_le_bytes());
    }
    /// `write(1, rsi, rdx)`: the buffer is in rsi and the length in rdx.
    // frame: `push rbp; mov rbp, rsp; sub rsp, imm32`. Returns the imm32 position.
    pub(super) fn prologue(&mut self) -> usize {
        self.emit(&[0x55]); // push rbp
        self.emit(&[0x48, 0x89, 0xE5]); // mov rbp, rsp
        self.emit(&[0x48, 0x81, 0xEC]); // sub rsp, imm32
        let pos = self.code.len();
        self.emit(&[0, 0, 0, 0]);
        pos
    }
    pub(super) fn patch_frame(&mut self, pos: usize, frame: i32) {
        self.code[pos..pos + 4].copy_from_slice(&frame.to_le_bytes());
    }
    pub(super) fn epilogue(&mut self) {
        self.emit(&[0x48, 0x89, 0xEC]); // mov rsp, rbp
        self.emit(&[0x5D]); // pop rbp
        self.emit(&[0xC3]); // ret
    }
    /// `mov rax, imm64`
    pub(super) fn mov_rax_imm(&mut self, v: i64) {
        self.emit(&[0x48, 0xB8]);
        self.emit(&v.to_le_bytes());
    }
    /// `lea rax, [rbp - off]` — the address of a local slot.
    pub(super) fn lea_local(&mut self, off: i32) {
        self.emit(&[0x48, 0x8D, 0x85]);
        self.emit(&(-off).to_le_bytes());
    }
    /// Width-aware load from `[rbp - off]` into rax (sign/zero extending narrow
    /// values per `signed`). ModRM `0x85` = `[rbp + disp32]`, reg field rax.
    pub(super) fn load_local(&mut self, off: i32, size: i32, signed: bool) {
        self.emit(load_opcode(size, signed));
        self.emit(&[0x85]);
        self.emit(&(-off).to_le_bytes());
    }
    /// Store the low `size` bytes of rax to `[rbp - off]`.
    pub(super) fn store_local(&mut self, off: i32, size: i32) {
        self.emit(store_opcode(size));
        self.emit(&[0x85]);
        self.emit(&(-off).to_le_bytes());
    }
    /// Width-aware load through the address in rax (`rax = [rax]`). ModRM `0x00`.
    pub(super) fn load_through(&mut self, size: i32, signed: bool) {
        self.emit(load_opcode(size, signed));
        self.emit(&[0x00]);
    }
    /// Store the low `size` bytes of rax through the address in rcx (`[rcx] = rax`).
    pub(super) fn store_through(&mut self, size: i32) {
        self.emit(store_opcode(size));
        self.emit(&[0x01]); // ModRM 0x01 = [rcx], reg field rax
    }
    pub(super) fn pop_rcx(&mut self) {
        self.emit(&[0x59]);
    }
    /// `imul rax, rax, imm32`
    pub(super) fn imul_rax_imm32(&mut self, imm: i32) {
        self.emit(&[0x48, 0x69, 0xC0]);
        self.emit(&imm.to_le_bytes());
    }
    /// `add rax, imm32`
    pub(super) fn add_rax_imm32(&mut self, imm: i32) {
        self.emit(&[0x48, 0x05]);
        self.emit(&imm.to_le_bytes());
    }
    /// `mov rax, <arg register i>` (System V order).
    pub(super) fn mov_rax_argreg(&mut self, i: usize) {
        match i {
            0 => self.emit(&[0x48, 0x89, 0xF8]), // mov rax, rdi
            1 => self.emit(&[0x48, 0x89, 0xF0]), // mov rax, rsi
            2 => self.emit(&[0x48, 0x89, 0xD0]), // mov rax, rdx
            3 => self.emit(&[0x48, 0x89, 0xC8]), // mov rax, rcx
            4 => self.emit(&[0x4C, 0x89, 0xC0]), // mov rax, r8
            5 => self.emit(&[0x4C, 0x89, 0xC8]), // mov rax, r9
            _ => unreachable!("at most 6 args"),
        }
    }
    /// `pop <arg register i>`
    pub(super) fn pop_argreg(&mut self, i: usize) {
        match i {
            0 => self.emit(&[0x5F]),       // pop rdi
            1 => self.emit(&[0x5E]),       // pop rsi
            2 => self.emit(&[0x5A]),       // pop rdx
            3 => self.emit(&[0x59]),       // pop rcx
            4 => self.emit(&[0x41, 0x58]), // pop r8
            5 => self.emit(&[0x41, 0x59]), // pop r9
            _ => unreachable!("at most 6 args"),
        }
    }
    pub(super) fn push_rax(&mut self) {
        self.emit(&[0x50]);
    }
    pub(super) fn pop_rax(&mut self) {
        self.emit(&[0x58]);
    }
    pub(super) fn mov_rcx_rax(&mut self) {
        self.emit(&[0x48, 0x89, 0xC1]);
    }
    /// `mov rcx, imm32` (sign-extended to 64 bits)
    pub(super) fn mov_rcx_imm32(&mut self, imm: i32) {
        self.emit(&[0x48, 0xC7, 0xC1]);
        self.emit(&imm.to_le_bytes());
    }
    pub(super) fn mov_rax_rdx(&mut self) {
        self.emit(&[0x48, 0x89, 0xD0]);
    }
    pub(super) fn mov_rdi_rax(&mut self) {
        self.emit(&[0x48, 0x89, 0xC7]);
    }
    pub(super) fn mov_rsi_rax(&mut self) {
        self.emit(&[0x48, 0x89, 0xC6]);
    }
    /// `rep movsb` — copy rcx bytes from [rsi] to [rdi].
    pub(super) fn rep_movsb(&mut self) {
        self.emit(&[0xF3, 0xA4]);
    }
    pub(super) fn add_rax_rcx(&mut self) {
        self.emit(&[0x48, 0x01, 0xC8]);
    }
    pub(super) fn sub_rax_rcx(&mut self) {
        self.emit(&[0x48, 0x29, 0xC8]);
    }
    pub(super) fn imul_rax_rcx(&mut self) {
        self.emit(&[0x48, 0x0F, 0xAF, 0xC1]);
    }
    pub(super) fn cqo(&mut self) {
        self.emit(&[0x48, 0x99]);
    }
    pub(super) fn idiv_rcx(&mut self) {
        self.emit(&[0x48, 0xF7, 0xF9]);
    }
    /// `xor edx, edx` then `div rcx` — unsigned divide rax by rcx (zero-extend).
    pub(super) fn div_rcx(&mut self) {
        self.emit(&[0x31, 0xD2]); // xor edx, edx (clears rdx)
        self.emit(&[0x48, 0xF7, 0xF1]); // div rcx
    }
    pub(super) fn and_rax_rcx(&mut self) {
        self.emit(&[0x48, 0x21, 0xC8]);
    }
    pub(super) fn or_rax_rcx(&mut self) {
        self.emit(&[0x48, 0x09, 0xC8]);
    }
    pub(super) fn xor_rax_rcx(&mut self) {
        self.emit(&[0x48, 0x31, 0xC8]);
    }
    pub(super) fn neg_rax(&mut self) {
        self.emit(&[0x48, 0xF7, 0xD8]);
    }
    pub(super) fn not_rax(&mut self) {
        self.emit(&[0x48, 0xF7, 0xD0]);
    }
    pub(super) fn shl_rax_cl(&mut self) {
        self.emit(&[0x48, 0xD3, 0xE0]);
    }
    pub(super) fn sar_rax_cl(&mut self) {
        self.emit(&[0x48, 0xD3, 0xF8]);
    }
    /// `shr rax, cl` — logical (zero-filling) right shift, for unsigned operands.
    pub(super) fn shr_rax_cl(&mut self) {
        self.emit(&[0x48, 0xD3, 0xE8]);
    }
    pub(super) fn test_rax(&mut self) {
        self.emit(&[0x48, 0x85, 0xC0]); // test rax, rax
    }
    /// `cmp rax, rcx` (sets EFLAGS for a following conditional jump).
    pub(super) fn cmp_rax_rcx(&mut self) {
        self.emit(&[0x48, 0x39, 0xC8]);
    }
    /// `cmp rax, rcx` then materialize the `setcc` condition as 0/1 in rax.
    pub(super) fn cmp_set(&mut self, setcc: u8) {
        self.cmp_rax_rcx();
        self.setcc_movzx(setcc);
    }
    /// `setcc al; movzx eax, al` — flags -> 0/1 in rax.
    pub(super) fn setcc_movzx(&mut self, setcc: u8) {
        self.emit(&[0x0F, setcc, 0xC0]); // setcc al
        self.emit(&[0x0F, 0xB6, 0xC0]); // movzx eax, al
    }

    // ---- generic register encoders (used by the print formatter) ----
    // Register numbering: rax0 rcx1 rdx2 rbx3 rsp4 rbp5 rsi6 rdi7 r8..r15 = 8..15.

    /// `mov dst, src` (64-bit).
    pub(super) fn mov_rr(&mut self, dst: u8, src: u8) {
        self.emit(&[rex_w(src, dst), 0x89, modrm_rr(src, dst)]);
    }
    /// `mov dst, imm32` (sign-extended to 64-bit).
    pub(super) fn mov_ri(&mut self, dst: u8, imm: i32) {
        self.emit(&[rex_b1(dst), 0xC7, 0xC0 | (dst & 7)]);
        self.emit(&imm.to_le_bytes());
    }
    /// `movabs dst, imm64` (a full 64-bit immediate).
    pub(super) fn mov_ri64(&mut self, dst: u8, imm: u64) {
        self.emit(&[0x48 | if dst >= 8 { 0x01 } else { 0 }, 0xB8 | (dst & 7)]);
        self.emit(&imm.to_le_bytes());
    }
    /// `shr dst, imm8` (logical right shift by a constant).
    pub(super) fn shr_ri(&mut self, dst: u8, imm: u8) {
        self.emit(&[rex_b1(dst), 0xC1, 0xC0 | (5 << 3) | (dst & 7), imm]);
    }
    /// `<op> dst, src` for an `r/m, r` ALU opcode (01 add, 29 sub, 09 or, 21 and,
    /// 31 xor, 39 cmp, 85 test) — i.e. `dst = dst <op> src` (cmp/test set flags).
    pub(super) fn alu_rr(&mut self, op: u8, dst: u8, src: u8) {
        self.emit(&[rex_w(src, dst), op, modrm_rr(src, dst)]);
    }
    pub(super) fn add_rr(&mut self, dst: u8, src: u8) {
        self.alu_rr(0x01, dst, src);
    }
    pub(super) fn sub_rr(&mut self, dst: u8, src: u8) {
        self.alu_rr(0x29, dst, src);
    }
    pub(super) fn xor_rr(&mut self, dst: u8, src: u8) {
        self.alu_rr(0x31, dst, src);
    }
    pub(super) fn and_rr(&mut self, dst: u8, src: u8) {
        self.alu_rr(0x21, dst, src);
    }
    /// `mov [rsp + disp8], src` (a local in the current stack frame).
    pub(super) fn store_rsp(&mut self, disp: i8, src: u8) {
        self.emit(&[
            0x48 | if src >= 8 { 0x04 } else { 0 },
            0x89,
            0x44 | ((src & 7) << 3),
            0x24,
            disp as u8,
        ]);
    }
    /// `mov dst, [rsp + disp8]`.
    pub(super) fn load_rsp(&mut self, dst: u8, disp: i8) {
        self.emit(&[
            0x48 | if dst >= 8 { 0x04 } else { 0 },
            0x8B,
            0x44 | ((dst & 7) << 3),
            0x24,
            disp as u8,
        ]);
    }
    /// `mov qword [rsp + disp8], imm32`.
    pub(super) fn store_rsp_imm(&mut self, disp: i8, imm: i32) {
        self.emit(&[0x48, 0xC7, 0x44, 0x24, disp as u8]);
        self.emit(&imm.to_le_bytes());
    }
    /// `sub rsp, imm8` / `add rsp, imm8` (small frame adjust).
    pub(super) fn sub_rsp(&mut self, n: u8) {
        self.emit(&[0x48, 0x83, 0xEC, n]);
    }
    pub(super) fn add_rsp(&mut self, n: u8) {
        self.emit(&[0x48, 0x83, 0xC4, n]);
    }
    pub(super) fn cmp_rr(&mut self, a: u8, b: u8) {
        self.alu_rr(0x39, a, b); // flags from a - b
    }
    pub(super) fn test_rr(&mut self, a: u8, b: u8) {
        self.alu_rr(0x85, a, b);
    }
    /// A group-1 immediate ALU op (`81 /ext id`): ext 0 add, 1 or, 5 sub, 7 cmp.
    pub(super) fn alu_ri(&mut self, ext: u8, rm: u8, imm: i32) {
        self.emit(&[rex_b1(rm), 0x81, 0xC0 | (ext << 3) | (rm & 7)]);
        self.emit(&imm.to_le_bytes());
    }
    pub(super) fn add_ri(&mut self, rm: u8, imm: i32) {
        self.alu_ri(0, rm, imm);
    }
    pub(super) fn or_ri(&mut self, rm: u8, imm: i32) {
        self.alu_ri(1, rm, imm);
    }
    pub(super) fn cmp_ri(&mut self, rm: u8, imm: i32) {
        self.alu_ri(7, rm, imm);
    }
    /// `test rm, imm32` (`F7 /0 id`).
    pub(super) fn test_ri(&mut self, rm: u8, imm: i32) {
        self.emit(&[rex_b1(rm), 0xF7, 0xC0 | (rm & 7)]);
        self.emit(&imm.to_le_bytes());
    }
    pub(super) fn neg_r(&mut self, rm: u8) {
        self.emit(&[rex_b1(rm), 0xF7, 0xC0 | (3 << 3) | (rm & 7)]);
    }
    /// `div rm` — unsigned divide rdx:rax by `rm` (quotient rax, remainder rdx).
    pub(super) fn div_r(&mut self, rm: u8) {
        self.emit(&[rex_b1(rm), 0xF7, 0xC0 | (6 << 3) | (rm & 7)]);
    }
    /// `mul rm` — unsigned multiply rax by `rm` (128-bit product in rdx:rax).
    pub(super) fn mul_r(&mut self, rm: u8) {
        self.emit(&[rex_b1(rm), 0xF7, 0xC0 | (4 << 3) | (rm & 7)]);
    }
    /// `adc rm, imm8` (add-with-carry a small immediate, sign-extended).
    pub(super) fn adc_ri8(&mut self, rm: u8, imm: i8) {
        self.emit(&[rex_b1(rm), 0x83, 0xC0 | (2 << 3) | (rm & 7), imm as u8]);
    }
    /// `or dst, src` (64-bit).
    pub(super) fn or_rr(&mut self, dst: u8, src: u8) {
        self.alu_rr(0x09, dst, src);
    }
    /// `shl dst, cl` (variable left shift).
    pub(super) fn shl_cl(&mut self, dst: u8) {
        self.emit(&[rex_b1(dst), 0xD3, 0xC0 | (4 << 3) | (dst & 7)]);
    }
    /// `shr dst, cl` (variable logical right shift).
    pub(super) fn shr_cl(&mut self, dst: u8) {
        self.emit(&[rex_b1(dst), 0xD3, 0xC0 | (5 << 3) | (dst & 7)]);
    }
    /// `mov dst, [base + idx*8]` — load a bignum limb. `base`/`idx` not rsp/rbp/r12/r13.
    pub(super) fn load_qword_idx8(&mut self, dst: u8, base: u8, idx: u8) {
        let rex = 0x48
            | if dst >= 8 { 0x04 } else { 0 }
            | if idx >= 8 { 0x02 } else { 0 }
            | if base >= 8 { 0x01 } else { 0 };
        self.emit(&[
            rex,
            0x8B,
            0x04 | ((dst & 7) << 3),
            0xC0 | ((idx & 7) << 3) | (base & 7),
        ]);
    }
    /// `mov [base + idx*8], src` — store a bignum limb.
    pub(super) fn store_qword_idx8(&mut self, base: u8, idx: u8, src: u8) {
        let rex = 0x48
            | if src >= 8 { 0x04 } else { 0 }
            | if idx >= 8 { 0x02 } else { 0 }
            | if base >= 8 { 0x01 } else { 0 };
        self.emit(&[
            rex,
            0x89,
            0x04 | ((src & 7) << 3),
            0xC0 | ((idx & 7) << 3) | (base & 7),
        ]);
    }
    pub(super) fn inc_r(&mut self, rm: u8) {
        self.emit(&[rex_b1(rm), 0xFF, 0xC0 | (rm & 7)]);
    }
    pub(super) fn dec_r(&mut self, rm: u8) {
        self.emit(&[rex_b1(rm), 0xFF, 0xC0 | (1 << 3) | (rm & 7)]);
    }
    /// `mov byte [base], src8` (low byte of `src`). `base` must not be rsp/rbp/r12/r13.
    pub(super) fn store_byte_at(&mut self, base: u8, src: u8) {
        // Always emit a REX so registers 4..7 mean spl/bpl/sil/dil (uniform byte regs).
        self.emit(&[
            0x40 | if src >= 8 { 0x04 } else { 0 } | if base >= 8 { 0x01 } else { 0 },
            0x88,
            ((src & 7) << 3) | (base & 7),
        ]);
    }
    /// `mov byte [base], imm8`.
    pub(super) fn store_byte_imm_at(&mut self, base: u8, imm: u8) {
        self.emit(&[0x40 | if base >= 8 { 0x01 } else { 0 }, 0xC6, base & 7, imm]);
    }
    /// `cmp byte [base], imm8`.
    pub(super) fn cmp_byte_imm_at(&mut self, base: u8, imm: u8) {
        self.emit(&[
            0x40 | if base >= 8 { 0x01 } else { 0 },
            0x80,
            (7 << 3) | (base & 7),
            imm,
        ]);
    }
    pub(super) fn and_ri(&mut self, rm: u8, imm: i32) {
        self.alu_ri(4, rm, imm);
    }
    /// `mov dst, [base]` (64-bit). `base` must not be rsp/rbp/r12/r13.
    pub(super) fn load_qword_at(&mut self, dst: u8, base: u8) {
        self.emit(&[rex_w(dst, base), 0x8B, ((dst & 7) << 3) | (base & 7)]);
    }
    /// `mov [base], src` (64-bit). `base` must not be rsp/rbp/r12/r13.
    pub(super) fn store_qword_at(&mut self, base: u8, src: u8) {
        self.emit(&[rex_w(src, base), 0x89, ((src & 7) << 3) | (base & 7)]);
    }
    /// `movzx dst, byte [base]` (zero-extend a byte to 64-bit).
    pub(super) fn load_byte_zx(&mut self, dst: u8, base: u8) {
        self.emit(&[
            0x40 | if dst >= 8 { 0x04 } else { 0 } | if base >= 8 { 0x01 } else { 0 },
            0x0F,
            0xB6,
            ((dst & 7) << 3) | (base & 7),
        ]);
    }
    /// `cmp <a>, <b>` (64-bit) — flags from `a - b`, for a following conditional jump.
    pub(super) fn cmp_reg_reg(&mut self, a: u8, b: u8) {
        self.cmp_rr(a, b);
    }
    pub(super) fn syscall(&mut self) {
        self.emit(&[0x0F, 0x05]);
    }

    // ---- SSE2 (F64) encoders. The expression evaluator uses xmm0 as the float
    // result and xmm1 as the temp; argument passing reaches xmm0..xmm7. ----

    /// `movq xmm_d, r_s` — move the 64 bits of a GPR into an xmm register.
    pub(super) fn movq_xmm_from_r(&mut self, xd: u8, rs: u8) {
        self.emit(&[0x66, rex_w(xd, rs), 0x0F, 0x6E, modrm_rr(xd, rs)]);
    }
    /// `movq r_d, xmm_s` — move the low 64 bits of an xmm register into a GPR.
    pub(super) fn movq_r_from_xmm(&mut self, rd: u8, xs: u8) {
        self.emit(&[0x66, rex_w(xs, rd), 0x0F, 0x7E, modrm_rr(xs, rd)]);
    }
    /// `btc rax, 63` — flip bit 63 (a double's sign bit ⇒ negation).
    pub(super) fn btc_rax_63(&mut self) {
        self.emit(&[0x48, 0x0F, 0xBA, 0xF8, 63]);
    }
    /// `movsd xmm_d, xmm_s`.
    pub(super) fn movsd_rr(&mut self, xd: u8, xs: u8) {
        self.sse_rr(0xF2, 0x10, xd, xs);
    }
    pub(super) fn addsd(&mut self, xd: u8, xs: u8) {
        self.sse_rr(0xF2, 0x58, xd, xs);
    }
    pub(super) fn subsd(&mut self, xd: u8, xs: u8) {
        self.sse_rr(0xF2, 0x5C, xd, xs);
    }
    pub(super) fn mulsd(&mut self, xd: u8, xs: u8) {
        self.sse_rr(0xF2, 0x59, xd, xs);
    }
    pub(super) fn divsd(&mut self, xd: u8, xs: u8) {
        self.sse_rr(0xF2, 0x5E, xd, xs);
    }
    /// `sqrtsd xmm_d, xmm_s` — emitted in place of a call to the lib `Sqrt` (the
    /// [`crate::intrinsics`] optimization).
    pub(super) fn sqrtsd(&mut self, xd: u8, xs: u8) {
        self.sse_rr(0xF2, 0x51, xd, xs);
    }
    /// `andpd xmm_d, xmm_s` — bitwise AND of doubles (for the lib `Fabs` optimization,
    /// masking off the sign bit).
    pub(super) fn andpd(&mut self, xd: u8, xs: u8) {
        // 66 0F 54 /r
        let mut bytes = vec![0x66u8];
        if xd >= 8 || xs >= 8 {
            bytes.push(0x40 | if xd >= 8 { 0x04 } else { 0 } | if xs >= 8 { 0x01 } else { 0 });
        }
        bytes.extend_from_slice(&[0x0F, 0x54, modrm_rr(xd, xs)]);
        self.emit(&bytes);
    }
    /// `ucomisd xmm_a, xmm_b` — set EFLAGS from an (unordered) double compare.
    pub(super) fn ucomisd(&mut self, xa: u8, xb: u8) {
        // 66 0F 2E /r (no F2/F3 prefix, no REX.W).
        let mut bytes = vec![0x66u8];
        if xa >= 8 || xb >= 8 {
            bytes.push(0x40 | if xa >= 8 { 0x04 } else { 0 } | if xb >= 8 { 0x01 } else { 0 });
        }
        bytes.extend_from_slice(&[0x0F, 0x2E, modrm_rr(xa, xb)]);
        self.emit(&bytes);
    }
    /// `cvtsi2sd xmm_d, r_s` — signed 64-bit integer to double.
    pub(super) fn cvtsi2sd(&mut self, xd: u8, rs: u8) {
        self.emit(&[0xF2, rex_w(xd, rs), 0x0F, 0x2A, modrm_rr(xd, rs)]);
    }
    /// `cvttsd2si r_d, xmm_s` — double to signed 64-bit integer (truncate).
    pub(super) fn cvttsd2si(&mut self, rd: u8, xs: u8) {
        self.emit(&[0xF2, rex_w(rd, xs), 0x0F, 0x2C, modrm_rr(rd, xs)]);
    }
    /// An `F2`/`66`-prefixed two-byte-opcode reg-reg SSE op (`reg = xd`, `rm = xs`).
    pub(super) fn sse_rr(&mut self, prefix: u8, op: u8, xd: u8, xs: u8) {
        let mut bytes = vec![prefix];
        if xd >= 8 || xs >= 8 {
            bytes.push(0x40 | if xd >= 8 { 0x04 } else { 0 } | if xs >= 8 { 0x01 } else { 0 });
        }
        bytes.extend_from_slice(&[0x0F, op, modrm_rr(xd, xs)]);
        self.emit(&bytes);
    }
    /// `movsd [rbp - off], xmm0`.
    pub(super) fn movsd_store_local(&mut self, off: i32) {
        self.movsd_local_xmm(0x11, off, 0);
    }
    /// `movsd [rbp - off], xmmN` (used to spill an argument register to its slot).
    pub(super) fn movsd_store_local_xmm(&mut self, off: i32, xmm: u8) {
        self.movsd_local_xmm(0x11, off, xmm);
    }
    /// A `movsd` between `xmm` and `[rbp - off]` (`op` 0x10 load / 0x11 store).
    pub(super) fn movsd_local_xmm(&mut self, op: u8, off: i32, xmm: u8) {
        self.emit(&[0xF2]);
        if xmm >= 8 {
            self.emit(&[0x44]); // REX.R
        }
        self.emit(&[0x0F, op, 0x85 | ((xmm & 7) << 3)]); // mod=10, rm=rbp(101)
        self.emit(&(-off).to_le_bytes());
    }
    /// `movsd xmm0, [base]` (base is a GPR holding an address; not rbp/rsp/r12/r13).
    pub(super) fn movsd_load_at(&mut self, base: u8) {
        let mut bytes = vec![0xF2u8];
        if base >= 8 {
            bytes.push(0x41);
        }
        bytes.extend_from_slice(&[0x0F, 0x10, base & 7]); // reg=xmm0, mod=00
        self.emit(&bytes);
    }
    /// `movsd [base], xmm0`.
    pub(super) fn movsd_store_at(&mut self, base: u8) {
        let mut bytes = vec![0xF2u8];
        if base >= 8 {
            bytes.push(0x41);
        }
        bytes.extend_from_slice(&[0x0F, 0x11, base & 7]);
        self.emit(&bytes);
    }
}

/// REX.W with REX.R/REX.B set per the reg (ModRM.reg) and rm (ModRM.r/m) numbers.
fn rex_w(reg: u8, rm: u8) -> u8 {
    0x48 | if reg >= 8 { 0x04 } else { 0 } | if rm >= 8 { 0x01 } else { 0 }
}
/// REX.W with only REX.B (for single-operand `r/m`-form instructions).
fn rex_b1(rm: u8) -> u8 {
    0x48 | if rm >= 8 { 0x01 } else { 0 }
}
/// ModRM byte for a register-direct (mod = 11) operand pair.
fn modrm_rr(reg: u8, rm: u8) -> u8 {
    0xC0 | ((reg & 7) << 3) | (rm & 7)
}
