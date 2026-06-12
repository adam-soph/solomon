//! The AArch64 instruction emitter. It encodes the ISA and knows nothing about
//! the target OS.
//!
//! `Asm` accumulates 32-bit instruction words plus labels, fixups, and symbolic
//! relocations. It runs the post-emission peephole pass, then resolves everything
//! in [`Asm::finish`] into a [`CodeImage`]: the `__text` bytes plus the
//! relocations a linker must resolve.
//!
//! The encoder is deliberately OS-agnostic. The Mach-O object format and the `cc`
//! link step live in the `darwin` policy module, reached from the parent. Keeping
//! the encoder here keeps OS/container specifics from leaking into code generation.
//!
//! `SymRef`/`RelKind` are ARM64 relocation *kinds*, not Mach-O encodings; the
//! `darwin` writer maps them to Mach-O reloc numbers.

use std::collections::HashMap;

use crate::backend::CodegenError;
use crate::backend::arm64::{
    B_BRANCH, B_CALL, B_NORMAL, B_RET, FP, GP_ALL, LR, RES, SP, T2, XZR, gpb,
};

/// Shift-type field (bits [23:22]) for the shifted-register data-processing forms
/// (`*_shifted` encoders). ROR (0b11) is not used.
pub(super) const SH_LSL: u32 = 0;
pub(super) const SH_LSR: u32 = 1;
pub(super) const SH_ASR: u32 = 2;

enum Fixup {
    B26,
    B19,
    /// ADR rd, label: the PC-relative address of a label in `__text` (a function
    /// entry). Used to take a function's address (`&Func`).
    Adr,
    /// A jump-table data word: the byte distance from the table's start label
    /// (`usize`, a label index) to the target case label. Written as a full word.
    TableRel(usize),
}

/// The symbol a relocation refers to.
///
/// `Extern(name)` is an undefined external symbol, such as a libc function like
/// `_printf` or `_strlen`. Its final symbol index is resolved late, once the
/// symbol-table layout is known. `Sym(i)` is an already-final symbol index, used
/// for a global.
#[derive(Clone, Copy)]
pub(super) enum SymRef {
    Extern(&'static str),
    Sym(u32),
}

#[derive(Clone, Copy)]
pub(super) enum RelKind {
    Branch26,
    Page21,
    PageOff12,
}

pub(super) struct CodeImage {
    pub(super) text: Vec<u8>,
    /// `(byte offset in __text, symbol, kind)` relocations the linker resolves.
    pub(super) relocs: Vec<(u32, SymRef, RelKind)>,
    /// Final byte offset of each placed label, indexed by label id; `None` if the
    /// label was never placed. Read *after* `finish` so it reflects the peephole
    /// pass's word removals. Defined-symbol offsets must come from here, not from a
    /// pre-`finish` `label_byte`.
    pub(super) label_bytes: Vec<Option<u64>>,
}

pub(super) struct Asm {
    words: Vec<u32>,
    label_pos: Vec<Option<usize>>,
    fixups: Vec<(usize, usize, Fixup)>,
    strings: Vec<Vec<u8>>,
    string_dedup: HashMap<Vec<u8>, usize>,
    adr_fixups: Vec<(usize, usize)>,
    /// Freestanding global-address ADRs, as `(word index, BSS byte offset)`.
    /// Resolved in `finish` to a PC-relative `ADR` pointing at the global's fixed
    /// address (`text_len + offset`). The BSS follows code+strings in the image.
    global_adr_fixups: Vec<(usize, u64)>,
    relocs: Vec<(usize, SymRef, RelKind)>,
    // Liveness tags, parallel to `words` (one entry per emitted instruction word).
    inst_def: Vec<i8>,    // GP register written (0..30), or -1 for none / multi-def
    inst_use: Vec<u32>,   // bitmask of GP registers read (over-approximated)
    inst_branch: Vec<u8>, // B_NORMAL / B_CALL / B_RET / B_BRANCH
}

impl Asm {
    pub(super) fn new() -> Self {
        Asm {
            words: Vec::new(),
            label_pos: Vec::new(),
            fixups: Vec::new(),
            strings: Vec::new(),
            string_dedup: HashMap::new(),
            adr_fixups: Vec::new(),
            global_adr_fixups: Vec::new(),
            relocs: Vec::new(),
            inst_def: Vec::new(),
            inst_use: Vec::new(),
            inst_branch: Vec::new(),
        }
    }

    /// Emits a word with conservative liveness tags: it defines nothing the
    /// peephole can use, reads everything, and acts as a barrier. Emitters with
    /// known register behavior call the tagged variants below, so the liveness scan
    /// can see through them. Anything left on this path is simply never optimized
    /// across.
    pub(super) fn emit(&mut self, word: u32) {
        self.words.push(word);
        self.inst_def.push(-1);
        self.inst_use.push(GP_ALL);
        self.inst_branch.push(B_BRANCH);
    }
    /// Emits a word with explicit liveness tags.
    pub(super) fn emit_du(&mut self, word: u32, def: i32, uses: u32, branch: u8) {
        self.words.push(word);
        self.inst_def.push(if (0..31).contains(&def) {
            def as i8
        } else {
            -1
        });
        self.inst_use.push(uses & GP_ALL);
        self.inst_branch.push(branch);
    }
    /// rd = f(rn, rm)
    pub(super) fn e_rrr(&mut self, word: u32, rd: u32, rn: u32, rm: u32) {
        self.emit_du(word, rd as i32, gpb(rn) | gpb(rm), B_NORMAL);
    }
    /// rd = f(rn)
    pub(super) fn e_rr(&mut self, word: u32, rd: u32, rn: u32) {
        self.emit_du(word, rd as i32, gpb(rn), B_NORMAL);
    }
    /// rd = a constant or a value from a non-GP source. Write-only as far as GP
    /// registers go.
    pub(super) fn e_wr(&mut self, word: u32, rd: u32) {
        self.emit_du(word, rd as i32, 0, B_NORMAL);
    }
    /// No GP destination; reads `uses`. Used for stores and compares.
    pub(super) fn e_use(&mut self, word: u32, uses: u32) {
        self.emit_du(word, -1, uses, B_NORMAL);
    }
    /// Touches no GP register the peephole tracks. Used for FP-only or SP-only ops.
    pub(super) fn e_nogp(&mut self, word: u32) {
        self.emit_du(word, -1, 0, B_NORMAL);
    }
    pub(super) fn new_label(&mut self) -> usize {
        self.label_pos.push(None);
        self.label_pos.len() - 1
    }
    pub(super) fn place(&mut self, id: usize) {
        self.label_pos[id] = Some(self.words.len());
    }
    pub(super) fn intern_string(&mut self, s: &str) -> usize {
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(0);
        if let Some(&i) = self.string_dedup.get(&bytes) {
            return i;
        }
        let i = self.strings.len();
        self.string_dedup.insert(bytes.clone(), i);
        self.strings.push(bytes);
        i
    }

    /// Reports whether GP register `reg` is dead immediately after `words[m]`. The
    /// register is a caller-saved scratch (x9/x10). It is dead if it is
    /// overwritten, clobbered by a call, or unused through the function's return,
    /// all before any read. The scan is conservative: it only follows fall-through,
    /// so any plain branch ends it with the register treated as live.
    pub(super) fn dead_after(&self, m: usize, reg: u32) -> bool {
        let bit = 1u32 << reg;
        let mut j = m + 1;
        while j < self.words.len() {
            match self.inst_branch[j] {
                B_NORMAL => {
                    if self.inst_use[j] & bit != 0 {
                        return false; // read -> live
                    }
                    if self.inst_def[j] == reg as i8 {
                        return true; // overwritten before any read -> dead
                    }
                }
                B_CALL => {
                    if self.inst_use[j] & bit != 0 {
                        return false; // the call is *through* this register
                    }
                    return true; // x9/x10 are caller-saved: the call clobbers them
                }
                B_RET => return true, // x9/x10 are not live-out of a function
                _ => return false,    // any other branch: conservative barrier
            }
            j += 1;
        }
        true
    }

    /// Liveness-driven dead-`mov` elimination, run once before fixups resolve.
    ///
    /// Removes `mov Xd, Xs` moves that can't change observable behavior, then
    /// remaps every stored word-index position past the removed words. It is
    /// restricted to the pure scratch temporaries x9 (RES) and x10 (T2). Those are
    /// never live across a call or return and are never ABI registers. Two cases:
    ///   * removal: Xd is dead after the move, so the copy is pointless.
    ///   * fusion: the immediately-preceding instruction produced Xs, and Xs is
    ///     dead after the move, so that instruction can target Xd directly.
    pub(super) fn peephole(&mut self) {
        let n = self.words.len();
        if n == 0 {
            return;
        }
        // A move a label points at is a branch target; never touch it.
        let mut label_here = vec![false; n + 1];
        for p in self.label_pos.iter().flatten() {
            if *p < label_here.len() {
                label_here[*p] = true;
            }
        }
        // Positions carrying a fixup/reloc. A `mov` never does, but a fused-into
        // predecessor might, so skip those to keep the rewrite reasoning simple.
        let mut protected = vec![false; n];
        for (at, _, _) in &self.fixups {
            protected[*at] = true;
        }
        for (at, _) in &self.adr_fixups {
            protected[*at] = true;
        }
        for (at, _, _) in &self.relocs {
            protected[*at] = true;
        }

        let mut remove = vec![false; n];
        for m in 0..n {
            let w = self.words[m];
            if (w & 0xFFE0_FFE0) != 0xAA00_03E0 {
                continue; // not `mov Xd, Xs` (ORR Xd, XZR, Xs, no shift)
            }
            if label_here[m] {
                continue;
            }
            let xd = w & 0x1F;
            let xs = (w >> 16) & 0x1F;
            if xd == xs {
                continue;
            }
            // Removal: a copy into a scratch register that is never read again.
            if (xd == RES || xd == T2) && self.dead_after(m, xd) {
                remove[m] = true;
                continue;
            }
            // Fusion: let the producer of Xs write Xd directly and drop the copy.
            if (xs == RES || xs == T2)
                && m >= 1
                && !remove[m - 1]
                && !protected[m - 1]
                && self.inst_def[m - 1] == xs as i8
                && (self.words[m - 1] & 0xFF80_0000) != 0xF280_0000 // movk reads its own Rd
                && self.dead_after(m, xs)
            {
                self.words[m - 1] = (self.words[m - 1] & !0x1F) | xd;
                self.inst_def[m - 1] = xd as i8;
                remove[m] = true;
            }
        }
        if !remove.iter().any(|&r| r) {
            return;
        }

        // Compact the word stream and remap every word-index position. Label ids are
        // resolved through label_pos, so only label_pos entries and the `.0` of
        // fixups/adr_fixups/relocs need to move.
        let mut shift = vec![0usize; n + 1];
        for i in 0..n {
            shift[i + 1] = shift[i] + remove[i] as usize;
        }
        let remap = |p: usize| p - shift[p];

        let keep =
            |v: &[u32]| -> Vec<u32> { (0..n).filter(|&i| !remove[i]).map(|i| v[i]).collect() };
        self.words = keep(&self.words);
        self.inst_use = keep(&self.inst_use);
        self.inst_def = (0..n)
            .filter(|&i| !remove[i])
            .map(|i| self.inst_def[i])
            .collect();
        self.inst_branch = (0..n)
            .filter(|&i| !remove[i])
            .map(|i| self.inst_branch[i])
            .collect();

        for lp in self.label_pos.iter_mut() {
            if let Some(p) = lp {
                *p = remap(*p);
            }
        }
        for f in self.fixups.iter_mut() {
            f.0 = remap(f.0);
        }
        for a in self.adr_fixups.iter_mut() {
            a.0 = remap(a.0);
        }
        for a in self.global_adr_fixups.iter_mut() {
            a.0 = remap(a.0);
        }
        for r in self.relocs.iter_mut() {
            r.0 = remap(r.0);
        }
    }

    pub(super) fn finish(mut self) -> Result<CodeImage, CodegenError> {
        self.peephole();
        for (at, id, kind) in &self.fixups {
            let target = self.label_pos[*id]
                .ok_or_else(|| CodegenError::new("internal: unplaced code label", None))?;
            let off = target as i64 - *at as i64;
            match kind {
                Fixup::B26 => self.words[*at] |= (off as u32) & 0x03FF_FFFF,
                Fixup::B19 => self.words[*at] |= ((off as u32) & 0x7_FFFF) << 5,
                Fixup::Adr => {
                    let imm = off * 4; // ADR immediate is in bytes
                    if !(-(1 << 20)..(1 << 20)).contains(&imm) {
                        return Err(CodegenError::new("function too far for ADR (>1MB)", None));
                    }
                    let immlo = (imm as u32) & 0x3;
                    let immhi = ((imm as u32) >> 2) & 0x7_FFFF;
                    self.words[*at] |= (immlo << 29) | (immhi << 5);
                }
                Fixup::TableRel(base) => {
                    let base_pos = self.label_pos[*base]
                        .ok_or_else(|| CodegenError::new("internal: unplaced table label", None))?;
                    // Byte distance table_base -> target (positions are word indices).
                    let off_bytes = (target as i64 - base_pos as i64) * 4;
                    self.words[*at] = off_bytes as u32; // a full data word, not a field
                }
            }
        }
        let code_bytes = self.words.len() * 4;
        let mut str_offsets = Vec::with_capacity(self.strings.len());
        let mut cursor = code_bytes;
        for s in &self.strings {
            str_offsets.push(cursor);
            cursor += s.len();
        }
        for (at, sidx) in &self.adr_fixups {
            let imm = str_offsets[*sidx] as i64 - (*at * 4) as i64;
            if !(-(1 << 20)..(1 << 20)).contains(&imm) {
                return Err(CodegenError::new("string too far for ADR (>1MB)", None));
            }
            let immlo = (imm as u32) & 0x3;
            let immhi = ((imm as u32) >> 2) & 0x7_FFFF;
            self.words[*at] |= (immlo << 29) | (immhi << 5);
        }
        // Freestanding globals live in the BSS that follows code+strings. The BSS
        // base is the end of code+strings, rounded up to a 16-byte boundary. The
        // load vaddr is page aligned, so an aligned base plus the per-global aligned
        // offsets give naturally aligned addresses. That alignment is required by
        // the AArch64 acquire/exclusive atomics, which fault on misalignment. The
        // image is zero-padded up to that base.
        let bss_base = if self.global_adr_fixups.is_empty() {
            cursor // Darwin (relocated globals) or no freestanding globals: unchanged
        } else {
            cursor.div_ceil(16) * 16
        };
        for (at, off) in &self.global_adr_fixups {
            let imm = (bss_base as i64 + *off as i64) - (*at * 4) as i64;
            if !(-(1 << 20)..(1 << 20)).contains(&imm) {
                return Err(CodegenError::new("global too far for ADR (>1MB)", None));
            }
            let immlo = (imm as u32) & 0x3;
            let immhi = ((imm as u32) >> 2) & 0x7_FFFF;
            self.words[*at] |= (immlo << 29) | (immhi << 5);
        }
        let mut text = Vec::with_capacity(bss_base);
        for w in &self.words {
            text.extend_from_slice(&w.to_le_bytes());
        }
        for s in &self.strings {
            text.extend_from_slice(s);
        }
        text.resize(bss_base, 0); // pad to the aligned BSS base
        let relocs = self
            .relocs
            .iter()
            .map(|(w, sym, kind)| ((*w * 4) as u32, *sym, *kind))
            .collect();
        let label_bytes = self
            .label_pos
            .iter()
            .map(|p| p.map(|w| (w * 4) as u64))
            .collect();
        Ok(CodeImage {
            text,
            relocs,
            label_bytes,
        })
    }

    // data processing
    pub(super) fn load_imm(&mut self, rd: u32, value: i64) {
        let v = value as u64;
        self.e_wr(0xD280_0000 | ((v as u32 & 0xFFFF) << 5) | rd, rd); // movz
        for hw in 1..4u32 {
            let half = ((v >> (16 * hw)) & 0xFFFF) as u32;
            if half != 0 {
                // movk read-modifies rd: its source register is also its
                // destination, so it must not be a fusion target.
                self.e_rr(0xF280_0000 | (hw << 21) | (half << 5) | rd, rd, rd);
            }
        }
    }
    pub(super) fn add(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0x8B00_0000 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    pub(super) fn sub(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0xCB00_0000 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    pub(super) fn mul(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(
            0x9B00_0000 | (rm << 16) | (XZR << 10) | (rn << 5) | rd,
            rd,
            rn,
            rm,
        );
    }
    pub(super) fn msub(&mut self, rd: u32, rn: u32, rm: u32, ra: u32) {
        let w = 0x9B00_8000 | (rm << 16) | (ra << 10) | (rn << 5) | rd;
        self.emit_du(w, rd as i32, gpb(rn) | gpb(rm) | gpb(ra), B_NORMAL);
    }
    /// MADD Xd, Xn, Xm, Xa: `Xd = Xa + Xn*Xm`, the fused integer multiply-add (one
    /// instruction, low-64-bit result — identical to a wrapping `mul` then `add`).
    pub(super) fn madd(&mut self, rd: u32, rn: u32, rm: u32, ra: u32) {
        let w = 0x9B00_0000 | (rm << 16) | (ra << 10) | (rn << 5) | rd;
        self.emit_du(w, rd as i32, gpb(rn) | gpb(rm) | gpb(ra), B_NORMAL);
    }
    pub(super) fn sdiv(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0x9AC0_0C00 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    pub(super) fn udiv(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0x9AC0_0800 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    pub(super) fn and(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0x8A00_0000 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    pub(super) fn orr(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0xAA00_0000 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    pub(super) fn eor(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0xCA00_0000 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    /// The add/sub/logical shifted-register forms compute `Xn <op> (Xm <shift> #imm6)` in one
    /// instruction. The shift-type field sits at bits [23:22] and the 6-bit amount at [15:10];
    /// the base words are the register forms above (which encode shift = LSL #0). `imm6` is the
    /// shift amount in 0..=63; `sh` is one of [`SH_LSL`]/[`SH_LSR`]/[`SH_ASR`].
    pub(super) fn add_shifted(&mut self, rd: u32, rn: u32, rm: u32, sh: u32, imm6: u32) {
        let w = 0x8B00_0000 | (sh << 22) | (rm << 16) | ((imm6 & 0x3F) << 10) | (rn << 5) | rd;
        self.e_rrr(w, rd, rn, rm);
    }
    pub(super) fn sub_shifted(&mut self, rd: u32, rn: u32, rm: u32, sh: u32, imm6: u32) {
        let w = 0xCB00_0000 | (sh << 22) | (rm << 16) | ((imm6 & 0x3F) << 10) | (rn << 5) | rd;
        self.e_rrr(w, rd, rn, rm);
    }
    pub(super) fn and_shifted(&mut self, rd: u32, rn: u32, rm: u32, sh: u32, imm6: u32) {
        let w = 0x8A00_0000 | (sh << 22) | (rm << 16) | ((imm6 & 0x3F) << 10) | (rn << 5) | rd;
        self.e_rrr(w, rd, rn, rm);
    }
    pub(super) fn orr_shifted(&mut self, rd: u32, rn: u32, rm: u32, sh: u32, imm6: u32) {
        let w = 0xAA00_0000 | (sh << 22) | (rm << 16) | ((imm6 & 0x3F) << 10) | (rn << 5) | rd;
        self.e_rrr(w, rd, rn, rm);
    }
    pub(super) fn eor_shifted(&mut self, rd: u32, rn: u32, rm: u32, sh: u32, imm6: u32) {
        let w = 0xCA00_0000 | (sh << 22) | (rm << 16) | ((imm6 & 0x3F) << 10) | (rn << 5) | rd;
        self.e_rrr(w, rd, rn, rm);
    }
    pub(super) fn lslv(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0x9AC0_2000 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    /// LDR Wt, [Xn]: loads the 32-bit word at `[base, #0]`, zero-extended.
    pub(super) fn ldr_w(&mut self, rt: u32, base: u32) {
        self.emit_du(
            0xB940_0000 | (base << 5) | rt,
            rt as i32,
            gpb(base),
            B_NORMAL,
        );
    }
    pub(super) fn lsrv(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0x9AC0_2400 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    /// ASRV Xd, Xn, Xm: arithmetic (sign-preserving) shift right by a register.
    pub(super) fn asrv(&mut self, rd: u32, rn: u32, rm: u32) {
        self.e_rrr(0x9AC0_2800 | (rm << 16) | (rn << 5) | rd, rd, rn, rm);
    }
    pub(super) fn neg(&mut self, rd: u32, rm: u32) {
        self.sub(rd, XZR, rm);
    }
    pub(super) fn mvn(&mut self, rd: u32, rm: u32) {
        self.e_rr(0xAA20_0000 | (rm << 16) | (XZR << 5) | rd, rd, rm);
    }
    pub(super) fn mov_reg(&mut self, rd: u32, rm: u32) {
        if rd != rm {
            self.orr(rd, XZR, rm); // a move to itself is a no-op
        }
    }
    /// SBFM Xd, Xn, #immr, #imms (used for sign-extend casts).
    pub(super) fn sbfm(&mut self, rd: u32, rn: u32, immr: u32, imms: u32) {
        self.e_rr(
            0x9340_0000 | (immr << 16) | (imms << 10) | (rn << 5) | rd,
            rd,
            rn,
        );
    }
    /// UBFM Xd, Xn, #immr, #imms (used for zero-extend casts).
    pub(super) fn ubfm(&mut self, rd: u32, rn: u32, immr: u32, imms: u32) {
        self.e_rr(
            0xD340_0000 | (immr << 16) | (imms << 10) | (rn << 5) | rd,
            rd,
            rn,
        );
    }

    /// FMOV Dd, Xn: moves raw 64 bits from a GPR into a double register.
    pub(super) fn fmov_from_gpr(&mut self, dd: u32, xn: u32) {
        self.e_use(0x9E67_0000 | (xn << 5) | dd, gpb(xn));
    }
    /// FMOV Xd, Dn: moves raw 64 bits from a double register into a GPR.
    pub(super) fn fmov_to_gpr(&mut self, xd: u32, dn: u32) {
        self.e_wr(0x9E66_0000 | (dn << 5) | xd, xd);
    }
    /// FMOV Dd, Dn: copies one double register to another.
    pub(super) fn fmov_reg(&mut self, dd: u32, dn: u32) {
        self.e_nogp(0x1E60_4000 | (dn << 5) | dd);
    }
    pub(super) fn fadd(&mut self, dd: u32, dn: u32, dm: u32) {
        self.e_nogp(0x1E60_2800 | (dm << 16) | (dn << 5) | dd);
    }
    /// CNT Vd.8B, Vn.8B: per-byte population count of the low 64 bits.
    pub(super) fn cnt_8b(&mut self, vd: u32, vn: u32) {
        self.e_nogp(0x0E20_5800 | (vn << 5) | vd);
    }
    /// ADDV Bd, Vn.8B: horizontal add of the eight bytes into a scalar.
    pub(super) fn addv_8b(&mut self, vd: u32, vn: u32) {
        self.e_nogp(0x0E31_B800 | (vn << 5) | vd);
    }
    pub(super) fn fsub(&mut self, dd: u32, dn: u32, dm: u32) {
        self.e_nogp(0x1E60_3800 | (dm << 16) | (dn << 5) | dd);
    }
    pub(super) fn fmul(&mut self, dd: u32, dn: u32, dm: u32) {
        self.e_nogp(0x1E60_0800 | (dm << 16) | (dn << 5) | dd);
    }
    pub(super) fn fdiv(&mut self, dd: u32, dn: u32, dm: u32) {
        self.e_nogp(0x1E60_1800 | (dm << 16) | (dn << 5) | dd);
    }
    pub(super) fn fneg(&mut self, dd: u32, dn: u32) {
        self.e_nogp(0x1E61_4000 | (dn << 5) | dd);
    }
    pub(super) fn frintz(&mut self, dd: u32, dn: u32) {
        self.e_nogp(0x1E65_C000 | (dn << 5) | dd);
    }
    /// FSQRT Dd, Dn: the IEEE square root (`Sqrt` lowered inline).
    pub(super) fn fsqrt(&mut self, dd: u32, dn: u32) {
        self.e_nogp(0x1E61_C000 | (dn << 5) | dd);
    }
    /// FABS Dd, Dn: the IEEE absolute value (`Fabs` lowered inline).
    pub(super) fn fabs(&mut self, dd: u32, dn: u32) {
        self.e_nogp(0x1E60_C000 | (dn << 5) | dd);
    }
    /// FRINTM Dd, Dn: round toward −∞ (`Floor`).
    pub(super) fn frintm(&mut self, dd: u32, dn: u32) {
        self.e_nogp(0x1E65_4000 | (dn << 5) | dd);
    }
    /// FRINTP Dd, Dn: round toward +∞ (`Ceil`).
    pub(super) fn frintp(&mut self, dd: u32, dn: u32) {
        self.e_nogp(0x1E64_C000 | (dn << 5) | dd);
    }
    /// FRINTA Dd, Dn: round to nearest, ties away from zero (`Round`).
    pub(super) fn frinta(&mut self, dd: u32, dn: u32) {
        self.e_nogp(0x1E66_4000 | (dn << 5) | dd);
    }
    /// FRINTN Dd, Dn: round to nearest, ties to even (`RoundToEven`).
    pub(super) fn frintn(&mut self, dd: u32, dn: u32) {
        self.e_nogp(0x1E64_4000 | (dn << 5) | dd);
    }
    /// FCMP Dn, Dm: sets NZCV for an ordered comparison.
    pub(super) fn fcmp(&mut self, dn: u32, dm: u32) {
        self.e_nogp(0x1E60_2000 | (dm << 16) | (dn << 5));
    }
    /// FCMP Dn, #0.0.
    pub(super) fn fcmp_zero(&mut self, dn: u32) {
        self.e_nogp(0x1E60_2008 | (dn << 5));
    }
    /// SCVTF Dd, Xn: signed 64-bit integer to double.
    pub(super) fn scvtf(&mut self, dd: u32, xn: u32) {
        self.e_use(0x9E62_0000 | (xn << 5) | dd, gpb(xn));
    }
    /// FCVTZS Xd, Dn: double to signed 64-bit integer, rounding toward zero.
    pub(super) fn fcvtzs(&mut self, xd: u32, dn: u32) {
        self.e_wr(0x9E78_0000 | (dn << 5) | xd, xd);
    }
    /// FCVTZU Xd, Dn: converts a double to a 64-bit *unsigned* integer, rounding
    /// toward zero and saturating. Used when the destination integer type is
    /// unsigned.
    pub(super) fn fcvtzu(&mut self, xd: u32, dn: u32) {
        self.e_wr(0x9E79_0000 | (dn << 5) | xd, xd);
    }

    pub(super) fn add_imm(&mut self, rd: u32, rn: u32, imm: u32) {
        self.e_rr(0x9100_0000 | ((imm & 0xFFF) << 10) | (rn << 5) | rd, rd, rn);
    }
    pub(super) fn sub_imm(&mut self, rd: u32, rn: u32, imm: u32) {
        self.e_rr(0xD100_0000 | ((imm & 0xFFF) << 10) | (rn << 5) | rd, rd, rn);
    }
    pub(super) fn add_sp_imm(&mut self, imm: u32) {
        if imm != 0 {
            self.add_imm(SP, SP, imm); // a 0-byte stack adjust is a no-op
        }
    }
    pub(super) fn sub_sp_imm(&mut self, imm: u32) {
        if imm != 0 {
            self.sub_imm(SP, SP, imm);
        }
    }

    // frame
    pub(super) fn stp_pre_fp_lr(&mut self) {
        // stp x29, x30, [sp, #-16]!
        let imm7 = (-2i32 as u32) & 0x7F;
        let w = 0xA980_0000 | (imm7 << 15) | (LR << 10) | (SP << 5) | FP;
        self.e_use(w, gpb(FP) | gpb(LR));
    }
    pub(super) fn ldp_post_fp_lr(&mut self) {
        // ldp x29, x30, [sp], #16. It writes x29/x30, neither of which is a
        // peephole candidate, so it is transparent to the x9/x10 liveness scan.
        self.e_nogp(0xA8C0_0000 | (2 << 15) | (LR << 10) | (SP << 5) | FP);
    }
    pub(super) fn mov_fp_sp(&mut self) {
        self.add_imm(FP, SP, 0);
    }
    pub(super) fn mov_sp_fp(&mut self) {
        self.add_imm(SP, FP, 0);
    }
    // Atomic / acquire-release memory ops (for `stdatomic.hc`). `sz` = log2(bytes),
    // placed in bits 31:30: 0=byte (`*b`), 1=half (`*h`), 2=word (32-bit),
    // 3=dword (64-bit). The narrow loads zero-extend into the 64-bit register; the
    // caller then sign/zero-extends per the pointee type. These ops use `emit`'s
    // conservative default tags (barrier, reads everything), which keep the
    // peephole pass from reordering or dropping them.
    /// LDAR{B,H} <t>, [Xn]: load-acquire.
    pub(super) fn ldar(&mut self, t: u32, n: u32, sz: u32) {
        self.emit((sz << 30) | 0x08DF_FC00 | (n << 5) | t);
    }
    /// STLR{B,H} <t>, [Xn]: store-release.
    pub(super) fn stlr(&mut self, t: u32, n: u32, sz: u32) {
        self.emit((sz << 30) | 0x089F_FC00 | (n << 5) | t);
    }
    /// LDAXR{B,H} <t>, [Xn]: load-acquire exclusive. Arms the exclusive monitor.
    pub(super) fn ldaxr(&mut self, t: u32, n: u32, sz: u32) {
        self.emit((sz << 30) | 0x085F_FC00 | (n << 5) | t);
    }
    /// STLXR{B,H} Ws, <t>, [Xn]: store-release exclusive. `Ws` is 0 on success, or
    /// 1 if the monitor was lost (retry).
    pub(super) fn stlxr(&mut self, s: u32, t: u32, n: u32, sz: u32) {
        self.emit((sz << 30) | 0x0800_FC00 | (s << 16) | (n << 5) | t);
    }
    /// DMB ISH: a full, inner-shareable data memory barrier. Implements `AtomicFence`.
    pub(super) fn dmb_ish(&mut self) {
        self.emit(0xD503_3BBF);
    }
    /// MRS Xd, TPIDR_EL0: read the user thread-pointer register. Freestanding threads
    /// carry their control-block base there (`CLONE_SETTLS`); the fresh process's main
    /// flow reads 0, which is how `ThreadExit` tells the two apart.
    pub(super) fn mrs_tpidr(&mut self, rd: u32) {
        self.e_wr(0xD53B_D040 | rd, rd);
    }

    // width-aware memory (offset 0 from `addr`)
    pub(super) fn load_mem(&mut self, dst: u32, addr: u32, size: u32, signed: bool) {
        self.load_mem_off(dst, addr, 0, size, signed);
    }
    pub(super) fn store_mem(&mut self, val: u32, addr: u32, size: u32) {
        self.store_mem_off(val, addr, 0, size);
    }
    /// Width-aware load from `[base, #byte_off]`. This is the unsigned-offset form,
    /// so `byte_off` must be a multiple of `size`.
    pub(super) fn load_mem_off(
        &mut self,
        dst: u32,
        base: u32,
        byte_off: u32,
        size: u32,
        signed: bool,
    ) {
        let op = match (size, signed) {
            (8, _) => 0xF940_0000,
            (4, true) => 0xB980_0000,
            (4, false) => 0xB940_0000,
            (2, true) => 0x7980_0000,
            (2, false) => 0x7940_0000,
            (1, true) => 0x3980_0000,
            (1, false) => 0x3940_0000,
            _ => 0xF940_0000,
        };
        let imm12 = (byte_off / size) & 0xFFF;
        self.e_rr(op | (imm12 << 10) | (base << 5) | dst, dst, base);
    }
    pub(super) fn store_mem_off(&mut self, val: u32, base: u32, byte_off: u32, size: u32) {
        let op = match size {
            8 => 0xF900_0000,
            4 => 0xB900_0000,
            2 => 0x7900_0000,
            1 => 0x3900_0000,
            _ => 0xF900_0000,
        };
        let imm12 = (byte_off / size) & 0xFFF;
        self.e_use(op | (imm12 << 10) | (base << 5) | val, gpb(val) | gpb(base));
    }
    /// STR rt, [sp, #off].
    pub(super) fn str_sp(&mut self, rt: u32, off: u32) {
        self.e_use(0xF900_0000 | ((off / 8) << 10) | (SP << 5) | rt, gpb(rt));
    }
    /// STR reg, [sp, #-16]! (push).
    pub(super) fn push(&mut self, reg: u32) {
        let imm9 = (-16i32 as u32) & 0x1FF;
        self.e_use(0xF800_0C00 | (imm9 << 12) | (SP << 5) | reg, gpb(reg));
    }
    /// LDR reg, [sp], #16 (pop).
    pub(super) fn pop(&mut self, reg: u32) {
        let imm9 = 16u32 & 0x1FF;
        self.e_wr(0xF840_0400 | (imm9 << 12) | (SP << 5) | reg, reg);
    }
    /// STUR Xt, [Xn, #simm9]: store at an unscaled signed byte offset. Used to
    /// spill a callee-saved register near x29 in one instruction.
    pub(super) fn stur(&mut self, rt: u32, base: u32, simm9: i32) {
        let imm = (simm9 as u32) & 0x1FF;
        self.e_use(
            0xF800_0000 | (imm << 12) | (base << 5) | rt,
            gpb(rt) | gpb(base),
        );
    }
    /// LDUR Xt, [Xn, #simm9]: load at an unscaled signed byte offset.
    pub(super) fn ldur(&mut self, rt: u32, base: u32, simm9: i32) {
        let imm = (simm9 as u32) & 0x1FF;
        self.e_rr(0xF840_0000 | (imm << 12) | (base << 5) | rt, rt, base);
    }
    /// STUR Dt, [Xn, #simm9]: spill a callee-saved double register near x29.
    pub(super) fn fstur(&mut self, dt: u32, base: u32, simm9: i32) {
        let imm = (simm9 as u32) & 0x1FF;
        self.e_use(0xFC00_0000 | (imm << 12) | (base << 5) | dt, gpb(base));
    }
    /// LDUR Dt, [Xn, #simm9]: reload a callee-saved double register.
    pub(super) fn fldur(&mut self, dt: u32, base: u32, simm9: i32) {
        let imm = (simm9 as u32) & 0x1FF;
        self.e_use(0xFC40_0000 | (imm << 12) | (base << 5) | dt, gpb(base));
    }

    pub(super) fn cmp_reg(&mut self, rn: u32, rm: u32) {
        self.e_use(
            0xEB00_0000 | (rm << 16) | (rn << 5) | XZR,
            gpb(rn) | gpb(rm),
        );
    }
    /// CMP Xn, #imm12: SUBS XZR, Xn, #imm. Sets flags and discards the result.
    pub(super) fn cmp_imm(&mut self, rn: u32, imm: u32) {
        self.e_use(
            0xF100_0000 | ((imm & 0xFFF) << 10) | (rn << 5) | XZR,
            gpb(rn),
        );
    }
    /// LSL/LSR/ASR Xd, Xn, #shift: shift by an immediate (aliases of UBFM/SBFM).
    pub(super) fn lsl_imm(&mut self, rd: u32, rn: u32, sh: u32) {
        self.ubfm(rd, rn, (64 - sh) & 63, 63 - sh);
    }
    pub(super) fn lsr_imm(&mut self, rd: u32, rn: u32, sh: u32) {
        self.ubfm(rd, rn, sh, 63);
    }
    /// ASR Xd, Xn, #shift: arithmetic (sign-preserving) shift right by an immediate, an
    /// alias of `SBFM Xd, Xn, #shift, #63`.
    pub(super) fn asr_imm(&mut self, rd: u32, rn: u32, sh: u32) {
        self.sbfm(rd, rn, sh, 63);
    }
    /// AND Xd, Xn, #imm with a pre-encoded logical (bitmask) immediate. `(n, immr, imms)`
    /// come from [`encode_logical_imm`].
    pub(super) fn and_imm(&mut self, rd: u32, rn: u32, n: u32, immr: u32, imms: u32) {
        self.e_rr(
            0x9200_0000 | (n << 22) | (immr << 16) | (imms << 10) | (rn << 5) | rd,
            rd,
            rn,
        );
    }
    /// ORR Xd, Xn, #imm with a pre-encoded logical immediate.
    pub(super) fn orr_imm(&mut self, rd: u32, rn: u32, n: u32, immr: u32, imms: u32) {
        self.e_rr(
            0xB200_0000 | (n << 22) | (immr << 16) | (imms << 10) | (rn << 5) | rd,
            rd,
            rn,
        );
    }
    /// EOR Xd, Xn, #imm with a pre-encoded logical immediate.
    pub(super) fn eor_imm(&mut self, rd: u32, rn: u32, n: u32, immr: u32, imms: u32) {
        self.e_rr(
            0xD200_0000 | (n << 22) | (immr << 16) | (imms << 10) | (rn << 5) | rd,
            rd,
            rn,
        );
    }
    /// AND Xd, Xn, #(2^k - 1): mask to the low `k` bits, for `k` in 1..=63. `2^k-1`
    /// is a run of `k` low ones, which encodes as the logical immediate N=1,
    /// immr=0, imms=k-1. Implements `x % 2^k` for unsigned `x`.
    pub(super) fn and_imm_lowbits(&mut self, rd: u32, rn: u32, k: u32) {
        self.e_rr(0x9240_0000 | ((k - 1) << 10) | (rn << 5) | rd, rd, rn);
    }
    pub(super) fn cset(&mut self, rd: u32, cond: u32) {
        let inv = cond ^ 1;
        self.e_wr(
            0x9A80_0400 | (XZR << 16) | (inv << 12) | (XZR << 5) | rd,
            rd,
        );
    }
    pub(super) fn ret(&mut self) {
        self.emit_du(0xD65F_03C0, -1, 0, B_RET);
    }

    pub(super) fn b(&mut self, label: usize) {
        self.fixups.push((self.words.len(), label, Fixup::B26));
        self.emit(0x1400_0000); // barrier (default tags)
    }
    pub(super) fn bl(&mut self, label: usize) {
        self.fixups.push((self.words.len(), label, Fixup::B26));
        self.emit_du(0x9400_0000, -1, 0, B_CALL);
    }
    /// BLR Xn: call the function whose entry address is in register `rn`.
    pub(super) fn blr(&mut self, rn: u32) {
        self.emit_du(0xD63F_0000 | (rn << 5), -1, gpb(rn), B_CALL);
    }
    /// BR Xn: unconditional branch to the address in `rn`, with no link. Used to
    /// jump into a branch table.
    pub(super) fn br(&mut self, rn: u32) {
        self.emit(0xD61F_0000 | (rn << 5)); // barrier (default tags)
    }
    /// SVC #0: a Linux syscall. The number goes in `x8`, args in `x0..x5`, and the
    /// result comes back in `x0`. Only the freestanding Linux target uses it.
    pub(super) fn svc(&mut self) {
        self.emit(0xD400_0001); // barrier (default tags)
    }
    /// ADR rd, label: load the PC-relative address of a label in `__text` (a
    /// function entry) into `rd`.
    pub(super) fn adr_label(&mut self, rd: u32, label: usize) {
        self.fixups.push((self.words.len(), label, Fixup::Adr));
        self.e_wr(0x1000_0000 | rd, rd);
    }
    /// LDRSW Xt, [base, index, LSL #2]: load a signed 32-bit word from a 4-byte-scaled
    /// index — the jump-table read (`Xt = (i64)table[index]`).
    pub(super) fn ldrsw_reg(&mut self, rt: u32, base: u32, index: u32) {
        let w = 0xB8A0_7800 | (index << 16) | (base << 5) | rt;
        self.emit_du(w, rt as i32, gpb(base) | gpb(index), B_NORMAL);
    }
    /// LDR <t>, [base, index{, LSL #log2(size)}]: a width-aware register-offset load.
    /// `scaled` picks `LSL #log2(size)` (index × element size) vs `LSL #0` (index × 1).
    /// Narrow loads sign/zero-extend into the 64-bit register per `signed`, matching
    /// [`load_mem`](Self::load_mem). Fuses an array element's address into the load.
    pub(super) fn load_reg(
        &mut self,
        dst: u32,
        base: u32,
        index: u32,
        size: u32,
        signed: bool,
        scaled: bool,
    ) {
        let sz = size.trailing_zeros(); // 1→0, 2→1, 4→2, 8→3
        let opc = if signed && size < 8 { 0b10 } else { 0b01 };
        let s = u32::from(scaled);
        let w =
            (sz << 30) | 0x3820_6800 | (opc << 22) | (s << 12) | (index << 16) | (base << 5) | dst;
        self.emit_du(w, dst as i32, gpb(base) | gpb(index), B_NORMAL);
    }
    /// STR <t>, [base, index{, LSL #log2(size)}]: the register-offset store mirror of
    /// [`load_reg`](Self::load_reg). Stores the low `size` bytes of `val`.
    pub(super) fn store_reg(&mut self, val: u32, base: u32, index: u32, size: u32, scaled: bool) {
        let sz = size.trailing_zeros();
        let s = u32::from(scaled);
        let w = (sz << 30) | 0x3820_6800 | (s << 12) | (index << 16) | (base << 5) | val;
        self.e_use(w, gpb(val) | gpb(base) | gpb(index));
    }
    /// Emit a 32-bit jump-table data word holding the byte distance from `base` (the
    /// table-start label) to `target` (a case label); resolved in `finish` via
    /// `Fixup::TableRel`. The word is data, not an instruction — it sits after a `br`,
    /// so it is never executed.
    pub(super) fn table_word(&mut self, base: usize, target: usize) {
        self.fixups
            .push((self.words.len(), target, Fixup::TableRel(base)));
        self.emit(0);
    }
    /// `bl <extern>`: a call to an undefined external (libc) symbol, resolved by
    /// the linker via a BRANCH26 relocation.
    pub(super) fn bl_extern(&mut self, sym: &'static str) {
        self.relocs
            .push((self.words.len(), SymRef::Extern(sym), RelKind::Branch26));
        self.emit_du(0x9400_0000, -1, 0, B_CALL);
    }
    /// ADR rd, <global>: load the PC-relative address of a freestanding global into
    /// `rd`. `bss_off` is the global's byte offset within the BSS that follows
    /// code+strings. Resolved in `finish`. This replaces the hosted
    /// `adrp_global`+`add_global` pair.
    pub(super) fn adr_global_fs(&mut self, rd: u32, bss_off: u64) {
        self.global_adr_fixups.push((self.words.len(), bss_off));
        self.e_wr(0x1000_0000 | rd, rd);
    }
    pub(super) fn adr(&mut self, rd: u32, sidx: usize) {
        self.adr_fixups.push((self.words.len(), sidx));
        self.e_wr(0x1000_0000 | rd, rd);
    }
    /// ADRP rd, sym@PAGE (the linker fills the immediate via a PAGE21 reloc).
    pub(super) fn adrp_global(&mut self, rd: u32, sym: u32) {
        self.relocs
            .push((self.words.len(), SymRef::Sym(sym), RelKind::Page21));
        self.e_wr(0x9000_0000 | rd, rd);
    }
    /// ADD rd, rn, sym@PAGEOFF (filled via a PAGEOFF12 reloc).
    pub(super) fn add_global(&mut self, rd: u32, rn: u32, sym: u32) {
        self.relocs
            .push((self.words.len(), SymRef::Sym(sym), RelKind::PageOff12));
        self.e_rr(0x9100_0000 | (rn << 5) | rd, rd, rn);
    }
    pub(super) fn b_cond(&mut self, cond: u32, label: usize) {
        self.fixups.push((self.words.len(), label, Fixup::B19));
        self.emit(0x5400_0000 | cond);
    }
    pub(super) fn cbz(&mut self, rt: u32, label: usize) {
        self.fixups.push((self.words.len(), label, Fixup::B19));
        self.emit(0xB400_0000 | rt);
    }
    pub(super) fn cbnz(&mut self, rt: u32, label: usize) {
        self.fixups.push((self.words.len(), label, Fixup::B19));
        self.emit(0xB500_0000 | rt);
    }
}

/// Whether `v` is a non-zero contiguous run of set bits ending at bit 0 (`0…01…1`).
fn is_mask(v: u64) -> bool {
    v != 0 && (v.wrapping_add(1) & v) == 0
}

/// Whether `v` is a non-zero contiguous run of set bits anywhere (`0…01…10…0`).
fn is_shifted_mask(v: u64) -> bool {
    v != 0 && is_mask((v - 1) | v)
}

/// Encode a 64-bit value as an AArch64 logical (bitmask) immediate `(N, immr, imms)`, or
/// `None` if it isn't a legal one. A legal bitmask immediate is a rotated, repeated run of
/// set bits; `0` and all-ones are not encodable (and are folded to identities upstream). This
/// is the standard `processLogicalImmediate` algorithm for the 64-bit register form. Used for
/// `AND`/`ORR`/`EOR` with a constant (e.g. `& 0x7FFFFFFF`) so no scratch materialize is needed.
pub(super) fn encode_logical_imm(imm: u64) -> Option<(u32, u32, u32)> {
    if imm == 0 || imm == u64::MAX {
        return None;
    }
    // 1. Element size: the smallest power-of-two period the pattern repeats over.
    let mut size = 64u32;
    loop {
        size /= 2;
        let mask = (1u64 << size) - 1;
        if (imm & mask) != ((imm >> size) & mask) {
            size *= 2;
            break;
        }
        if size <= 2 {
            break;
        }
    }
    // 2. Within one element, the value must be a rotated run of ones.
    let mask = u64::MAX >> (64 - size);
    let mut e = imm & mask;
    let (i, cto);
    if is_shifted_mask(e) {
        i = e.trailing_zeros();
        cto = (e >> i).trailing_ones();
    } else {
        e |= !mask;
        if !is_shifted_mask(!e) {
            return None;
        }
        let clo = e.leading_ones();
        i = 64 - clo;
        cto = clo + e.trailing_ones() - (64 - size);
    }
    let immr = (size - i) & (size - 1);
    let mut nimms = (!(size - 1)) << 1;
    nimms |= cto - 1;
    let n = ((nimms >> 6) & 1) ^ 1;
    Some((n, immr, nimms & 0x3f))
}

#[cfg(test)]
#[path = "tests/asm.rs"]
mod tests;
