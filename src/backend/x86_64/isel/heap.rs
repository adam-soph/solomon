//! The freestanding heap runtime: a bump allocator over OS page allocations, emitted as raw
//! `Asm` and called from `emit_heap_prim`. `hp`/`he` are the BSS offsets of the bump pointer
//! and chunk end; `uses_msize` reserves an 8-byte size header per block. The fresh-chunk
//! grab goes through the `OsTarget` page-alloc seam (mmap on Linux, `VirtualAlloc` on
//! Windows). Split out of `isel.rs`.

use crate::backend::x86_64::isel::*;

/// Emit the heap routines the program calls, each at its label.
pub(super) fn emit_heap_runtime(
    asm: &mut Asm,
    os: &mut dyn OsTarget,
    labels: &HashMap<&'static str, usize>,
    hp: i32,
    he: i32,
    uses_msize: bool,
) {
    if let Some(&l) = labels.get("MAlloc") {
        asm.place(l);
        emit_rt_malloc(asm, os, hp, he, uses_msize);
    }
    if let Some(&l) = labels.get("HeapExtend") {
        asm.place(l);
        emit_rt_heapextend(asm, hp, he, uses_msize);
    }
    if let Some(&l) = labels.get("MSize") {
        asm.place(l);
        emit_rt_msize(asm);
    }
    if let Some(&l) = labels.get("Free") {
        asm.place(l);
        asm.emit(&[0xC3]); // a no-op bump allocator never frees
    }
}

/// `MAlloc(rdi=n) -> rax`: bump allocator over OS page chunks (≥1 MiB, page-aligned).
fn emit_rt_malloc(asm: &mut Asm, os: &mut dyn OsTarget, hp: i32, he: i32, uses_msize: bool) {
    let alloc = asm.new_label();
    let sized = asm.new_label();
    asm.push_reg(RBX); // preserve rbx (survives the page-alloc call)
    if uses_msize {
        asm.push_reg(RDI); // keep the original n for the header
    }
    asm.add_ri(RDI, 15);
    asm.and_ri(RDI, -16);
    if uses_msize {
        asm.add_ri(RDI, 16); // reserve a 16-byte size header
    }
    asm.mov_rr(RBX, RDI); // rbx = total bytes to bump
    asm.lea_global(R9, hp);
    asm.load_qword_at(RAX, R9); // rax = *heap_ptr
    asm.lea_global(R10, he);
    asm.load_qword_at(R8, R10); // r8 = *heap_end
    asm.mov_rr(RCX, RAX);
    asm.add_rr(RCX, RBX); // rcx = ptr + n
    asm.cmp_reg_reg(RCX, R8);
    asm.jbe(alloc); // fits in the current chunk
    asm.mov_rr(RSI, RBX);
    asm.mov_ri(RCX, crate::backend::HEAP_CHUNK_SIZE as i32);
    asm.cmp_reg_reg(RSI, RCX);
    asm.jae(sized);
    asm.mov_rr(RSI, RCX);
    asm.place(sized);
    asm.add_ri(RSI, 4095);
    asm.and_ri(RSI, -4096);
    os.emit_page_alloc(asm); // base -> rax, rsi kept
    asm.mov_rr(R8, RAX);
    asm.add_rr(R8, RSI); // r8 = base + chunk size
    asm.lea_global(R10, he);
    asm.store_qword_at(R10, R8); // *heap_end = base + size
    asm.place(alloc);
    asm.mov_rr(RCX, RAX);
    asm.add_rr(RCX, RBX);
    asm.lea_global(R9, hp);
    asm.store_qword_at(R9, RCX); // *heap_ptr = base + n
    if uses_msize {
        asm.pop_reg(RCX); // the original n
        asm.store_qword_at(RAX, RCX); // [base] = n (the size header)
        asm.add_ri(RAX, 16); // return base + 16
    }
    asm.pop_reg(RBX);
    asm.emit(&[0xC3]); // ret
}

/// `HeapExtend(rdi=ptr, rsi=old, rdx=new) -> rax`: grow the bump allocator's last block in
/// place when it still fits the chunk, else NULL.
fn emit_rt_heapextend(asm: &mut Asm, hp: i32, he: i32, uses_msize: bool) {
    let null = asm.new_label();
    asm.test_rr(RDI, RDI);
    asm.je(null);
    asm.mov_rr(RAX, RSI);
    asm.add_ri(RAX, 15);
    asm.and_ri(RAX, -16);
    asm.mov_rr(RCX, RDX);
    asm.add_ri(RCX, 15);
    asm.and_ri(RCX, -16);
    asm.mov_rr(R8, RDI);
    asm.add_rr(R8, RAX); // r8 = block end
    asm.lea_global(R9, hp);
    asm.load_qword_at(R10, R9); // r10 = *heap_ptr
    asm.cmp_reg_reg(R8, R10);
    asm.jne(null);
    asm.mov_rr(R8, RDI);
    asm.add_rr(R8, RCX); // r8 = ptr + align16(new)
    asm.lea_global(R11, he);
    asm.load_qword_at(RAX, R11); // rax = *heap_end
    asm.cmp_reg_reg(RAX, R8);
    asm.jb(null);
    asm.store_qword_at(R9, R8); // *heap_ptr = ptr + anew
    if uses_msize {
        asm.mov_rr(RCX, RDI);
        asm.add_ri(RCX, -16);
        asm.store_qword_at(RCX, RDX); // [ptr-16] = new size
    }
    asm.mov_rr(RAX, RDI);
    asm.emit(&[0xC3]);
    asm.place(null);
    asm.mov_ri(RAX, 0);
    asm.emit(&[0xC3]);
}

/// `MSize(rdi=ptr) -> rax`: the requested size in `ptr`'s header (`*(ptr-16)`), 0 for NULL.
fn emit_rt_msize(asm: &mut Asm) {
    let nz = asm.new_label();
    asm.test_rr(RDI, RDI);
    asm.jne(nz);
    asm.mov_ri(RAX, 0);
    asm.emit(&[0xC3]);
    asm.place(nz);
    asm.mov_rr(RAX, RDI);
    asm.add_ri(RAX, -16);
    asm.load_qword_at(RAX, RAX); // rax = *(ptr - 16)
    asm.emit(&[0xC3]);
}
