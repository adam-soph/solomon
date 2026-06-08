//! The freestanding `aarch64-unknown-linux` target: a self-contained static ELF
//! with its own `_start` and raw syscalls. There is no libc and no linker. This
//! is the AArch64 analogue of the freestanding `x86_64-unknown-linux` backend.
//!
//! The AArch64 instruction encoding and the code generation are shared with the
//! Darwin backend, via the [`asm`](super::asm) module and the parent's `compile`
//! driver. This module supplies only the ELF-executable container: a single
//! `PT_LOAD` (R+W+X) over the emitted code, a trailing zero-filled BSS, and the
//! entry at the emitted `_start`.

use std::path::PathBuf;

use super::ArmTarget;
use crate::ast::Program;
use crate::codegen::{Codegen, CodegenError};

/// Compiles a HolyC program for `aarch64-unknown-linux`: a freestanding static
/// ELF with no libc and no linker. The AArch64 analogue of the freestanding
/// `x86_64-unknown-linux` backend.
pub struct Arm64Linux {
    out_path: PathBuf,
}

impl Arm64Linux {
    pub fn new(out_path: impl Into<PathBuf>) -> Self {
        Arm64Linux {
            out_path: out_path.into(),
        }
    }

    /// Emits the freestanding ELF executable for `program` as raw bytes. Exposed
    /// so structural tests can byte-check the image on any host. For a freestanding
    /// target, `compile` produces the runnable image directly.
    pub fn object(&self, program: &Program) -> Result<Vec<u8>, CodegenError> {
        super::emit_ir::compile_ir(program, &Linux)
    }
}

impl Codegen for Arm64Linux {
    fn name(&self) -> &'static str {
        "aarch64-unknown-linux"
    }

    fn run(&mut self, program: &Program) -> Result<(), CodegenError> {
        super::build(program, &self.out_path, &Linux)
    }
}

/// The freestanding Linux target policy: emit a self-contained static ELF.
struct Linux;

impl ArmTarget for Linux {
    fn freestanding(&self) -> bool {
        true
    }

    fn write_executable(&self, code: &[u8], bss: u64) -> Vec<u8> {
        write_elf_exec(code, bss)
    }
}

// ---- freestanding ELF executable writer ----

const VADDR: u64 = 0x40_0000;
const EHSIZE: u64 = 64;
const PHENTSIZE: u64 = 56;

/// Wraps freestanding `code` in a minimal static ELF64 executable. The image is
/// an ELF header followed by one `PT_LOAD` (R+W+X) covering the whole file, plus a
/// trailing zero-filled BSS region of `bss` bytes. The entry is the first code
/// byte, the emitted `_start`. Mirrors the `x86_64-unknown-linux` writer, with
/// `e_machine = EM_AARCH64`.
fn write_elf_exec(code: &[u8], bss: u64) -> Vec<u8> {
    let entry = VADDR + EHSIZE + PHENTSIZE;
    let filesz = EHSIZE + PHENTSIZE + code.len() as u64;
    let memsz = filesz + bss;
    let mut out = Vec::with_capacity(filesz as usize);

    out.extend_from_slice(&[0x7F, b'E', b'L', b'F']);
    out.push(2); // ELFCLASS64
    out.push(1); // ELFDATA2LSB
    out.push(1); // EI_VERSION
    out.push(0); // ELFOSABI_SYSV
    out.extend_from_slice(&[0u8; 8]);
    put_u16(&mut out, 2); // e_type = ET_EXEC
    put_u16(&mut out, 183); // e_machine = EM_AARCH64
    put_u32(&mut out, 1); // e_version
    put_u64(&mut out, entry); // e_entry
    put_u64(&mut out, EHSIZE); // e_phoff
    put_u64(&mut out, 0); // e_shoff
    put_u32(&mut out, 0); // e_flags
    put_u16(&mut out, EHSIZE as u16); // e_ehsize
    put_u16(&mut out, PHENTSIZE as u16); // e_phentsize
    put_u16(&mut out, 1); // e_phnum
    put_u16(&mut out, 0); // e_shentsize
    put_u16(&mut out, 0); // e_shnum
    put_u16(&mut out, 0); // e_shstrndx

    put_u32(&mut out, 1); // p_type = PT_LOAD
    put_u32(&mut out, 7); // p_flags = R | W | X
    put_u64(&mut out, 0); // p_offset
    put_u64(&mut out, VADDR); // p_vaddr
    put_u64(&mut out, VADDR); // p_paddr
    put_u64(&mut out, filesz); // p_filesz
    put_u64(&mut out, memsz); // p_memsz (file image + BSS)
    put_u64(&mut out, 0x1000); // p_align

    out.extend_from_slice(code);
    debug_assert_eq!(out.len() as u64, filesz);
    out
}

fn put_u16(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_u32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_le_bytes());
}
