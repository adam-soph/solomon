//! The freestanding `aarch64-unknown-linux` heap runtime: an `mmap`-backed bump allocator.
//!
//! One bump allocator: 16-byte-aligned allocations, two BSS state words for the bump pointer
//! and chunk end; `Free` is a no-op so chunks are never reused. Emitted as raw `Asm` and
//! called via `bl` from `emit_heap_prim`. `hp`/`he` are the BSS offsets of the bump pointer
//! and chunk end; `uses_msize` reserves an 8-byte size header per block. Split out of
//! `isel.rs`; Darwin maps the heap primitives to libc instead and emits none of this.

use crate::backend::arm64::isel::*;

const HS_HI: u32 = 0b1000; // unsigned higher (>)
const HS_LS: u32 = 0b1001; // unsigned lower-or-same (<=)
const HS_HS: u32 = 0b0010; // unsigned higher-or-same (>=)
const HS_NE: u32 = 0b0001;

/// Emit the heap routines the program calls, each at its label.
pub(super) fn emit_heap_runtime(
    asm: &mut Asm,
    labels: &HashMap<&'static str, usize>,
    hp: u64,
    he: u64,
    uses_msize: bool,
) {
    if let Some(&l) = labels.get("MAlloc") {
        asm.place(l);
        emit_fs_malloc(asm, hp, he, uses_msize);
    }
    if let Some(&l) = labels.get("HeapExtend") {
        asm.place(l);
        emit_fs_heapextend(asm, hp, he, uses_msize);
    }
    if let Some(&l) = labels.get("MSize") {
        asm.place(l);
        emit_fs_msize(asm);
    }
    if let Some(&l) = labels.get("Free") {
        asm.place(l);
        asm.ret(); // a no-op bump allocator never frees
    }
}

/// `MAlloc(x0=n) -> x0`: a bump allocator over `mmap`'d chunks (≥1 MiB, page-aligned).
fn emit_fs_malloc(asm: &mut Asm, hp: u64, he: u64, uses_msize: bool) {
    let fits = asm.new_label();
    let sized = asm.new_label();
    if uses_msize {
        asm.push(0); // save the original n for the size header
    }
    // x9 = (n + 15) & ~15
    asm.add_imm(9, 0, 15);
    asm.load_imm(10, -16);
    asm.and(9, 9, 10);
    if uses_msize {
        asm.add_imm(9, 9, 16); // reserve a 16-byte size header
    }
    // x11 = *hp, x12 = *he
    asm.adr_global_fs(13, hp);
    asm.load_mem(11, 13, 8, false);
    asm.adr_global_fs(14, he);
    asm.load_mem(12, 14, 8, false);
    asm.add(15, 11, 9); // hp + n
    asm.cmp_reg(15, 12);
    asm.b_cond(HS_LS, fits); // fits in the current chunk
    // chunk size x1 = max(n, 1 MiB), rounded up to a page
    asm.mov_reg(1, 9);
    asm.load_imm(10, crate::backend::HEAP_CHUNK_SIZE as i64);
    asm.cmp_reg(1, 10);
    asm.b_cond(HS_HS, sized);
    asm.mov_reg(1, 10);
    asm.place(sized);
    asm.add_imm(1, 1, 4095);
    asm.load_imm(10, -4096);
    asm.and(1, 1, 10);
    // mmap(0, x1, PROT_READ|WRITE=3, MAP_PRIVATE|ANON=0x22, -1, 0), nr 222.
    asm.load_imm(0, 0);
    asm.load_imm(2, 3);
    asm.load_imm(3, 0x22);
    asm.load_imm(4, -1);
    asm.load_imm(5, 0);
    asm.load_imm(8, 222);
    asm.svc();
    asm.mov_reg(11, 0); // hp base = mmap base
    asm.add(12, 0, 1); // he = base + chunk size
    asm.adr_global_fs(14, he);
    asm.store_mem(12, 14, 8);
    asm.place(fits);
    // result = x11 (base); *hp = base + n
    asm.add(15, 11, 9);
    asm.adr_global_fs(13, hp);
    asm.store_mem(15, 13, 8);
    if uses_msize {
        asm.pop(10); // x10 = original n
        asm.store_mem(10, 11, 8); // [base] = n (the size header)
        asm.add_imm(0, 11, 16); // return base + 16 (past the header)
    } else {
        asm.mov_reg(0, 11);
    }
    asm.ret();
}

/// `HeapExtend(x0=ptr, x1=old, x2=new) -> x0`: grow `ptr` in place when it is the last
/// bump-allocated block and still fits the chunk; else NULL.
fn emit_fs_heapextend(asm: &mut Asm, hp: u64, he: u64, uses_msize: bool) {
    let null = asm.new_label();
    asm.cbz(0, null); // NULL ptr never extends
    // x9 = align16(old), x11 = align16(new)
    asm.add_imm(9, 1, 15);
    asm.load_imm(10, -16);
    asm.and(9, 9, 10);
    asm.add_imm(11, 2, 15);
    asm.and(11, 11, 10);
    // last block? ptr + align16(old) == *heap_ptr
    asm.add(12, 0, 9);
    asm.adr_global_fs(13, hp);
    asm.load_mem(14, 13, 8, false);
    asm.cmp_reg(12, 14);
    asm.b_cond(HS_NE, null);
    // fits? ptr + align16(new) <= *heap_end
    asm.add(12, 0, 11);
    asm.adr_global_fs(14, he);
    asm.load_mem(15, 14, 8, false);
    asm.cmp_reg(12, 15);
    asm.b_cond(HS_HI, null); // ptr+anew > heap_end ⇒ doesn't fit
    // extend in place: *heap_ptr = ptr + anew; return ptr (x0 unchanged)
    asm.store_mem(12, 13, 8);
    if uses_msize {
        asm.sub_imm(9, 0, 16); // x9 = ptr - 16 (header)
        asm.store_mem(2, 9, 8); // keep MSize current: [ptr-16] = new size
    }
    asm.ret();
    asm.place(null);
    asm.load_imm(0, 0); // NULL
    asm.ret();
}

/// `MSize(x0=ptr) -> x0`: the requested byte size from `ptr`'s header (`*(ptr-16)`).
fn emit_fs_msize(asm: &mut Asm) {
    let null = asm.new_label();
    asm.cbz(0, null);
    asm.sub_imm(9, 0, 16);
    asm.load_mem(0, 9, 8, false); // x0 = *(ptr - 16)
    asm.ret();
    asm.place(null);
    asm.load_imm(0, 0); // MSize(NULL) == 0
    asm.ret();
}
