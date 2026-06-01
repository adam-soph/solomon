//! The `x86_64-unknown-linux` target: raw Linux syscalls and a freestanding
//! static ELF executable. The code generation itself is shared (see the parent
//! module); this module supplies only the [`OsTarget`] policy — the syscall
//! sequences for exit / page allocation / the stdout sink — and the ELF writer.

use std::path::PathBuf;

use super::{Asm, OsTarget, R8, R9, R10, RAX, RDI, RDX};
use crate::ast::Program;
use crate::codegen::{Codegen, CodegenError};

/// Compiles a HolyC program to a freestanding static ELF for `x86_64-unknown-linux`.
pub struct X64Linux {
    out_path: PathBuf,
}

impl X64Linux {
    pub fn new(out_path: impl Into<PathBuf>) -> Self {
        X64Linux {
            out_path: out_path.into(),
        }
    }
}

impl Codegen for X64Linux {
    fn name(&self) -> &'static str {
        "x86_64-unknown-linux"
    }

    fn run(&mut self, program: &Program) -> Result<(), CodegenError> {
        let elf = super::compile(program, Box::new(LinuxTarget))?;
        std::fs::write(&self.out_path, &elf)
            .map_err(|e| CodegenError::new(format!("cannot write ELF executable: {e}"), None))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(&self.out_path, std::fs::Permissions::from_mode(0o755));
        }
        Ok(())
    }
}

/// The Linux OS policy: Linux/x86-64 syscalls and a static-ELF container.
struct LinuxTarget;

impl OsTarget for LinuxTarget {
    fn emit_exit(&mut self, asm: &mut Asm) {
        // exit(status in rax): mov rdi, rax; mov rax, 60; syscall.
        asm.mov_rdi_rax();
        asm.mov_rax_imm(60);
        asm.syscall();
    }

    fn emit_page_alloc(&mut self, asm: &mut Asm) {
        // mmap(0, rsi, PROT_READ|WRITE, MAP_PRIVATE|ANON, -1, 0) — syscall 9.
        // Returns the base in rax; rsi (length) is preserved across the syscall.
        asm.mov_ri(RDI, 0);
        asm.mov_ri(RDX, 3);
        asm.mov_ri(R10, 0x22);
        asm.mov_ri(R8, -1);
        asm.mov_ri(R9, 0);
        asm.mov_ri(RAX, 9);
        asm.syscall();
    }

    fn emit_write_stdout(&mut self, asm: &mut Asm) {
        // write(1, rsi, rdx).
        asm.emit(&[0xB8, 1, 0, 0, 0]); // mov eax, 1 (SYS_write)
        asm.emit(&[0xBF, 1, 0, 0, 0]); // mov edi, 1 (stdout)
        asm.syscall();
    }

    fn wrap(&mut self, image: Vec<u8>, bss: u64) -> Vec<u8> {
        write_elf(&image, bss)
    }
}

// ---- ELF executable writer ----

const VADDR: u64 = 0x40_0000;
const EHSIZE: u64 = 64;
const PHENTSIZE: u64 = 56;

/// Wrap `code` in a minimal static ELF64 executable: ELF header + one PT_LOAD
/// (R+W+X) covering the whole file plus a trailing zero-filled BSS region of
/// `bss` bytes (globals + print scratch), the entry at the first code byte.
fn write_elf(code: &[u8], bss: u64) -> Vec<u8> {
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
    out.extend_from_slice(&2u16.to_le_bytes()); // e_type = ET_EXEC
    out.extend_from_slice(&0x3Eu16.to_le_bytes()); // e_machine = EM_X86_64
    out.extend_from_slice(&1u32.to_le_bytes()); // e_version
    out.extend_from_slice(&entry.to_le_bytes()); // e_entry
    out.extend_from_slice(&EHSIZE.to_le_bytes()); // e_phoff
    out.extend_from_slice(&0u64.to_le_bytes()); // e_shoff
    out.extend_from_slice(&0u32.to_le_bytes()); // e_flags
    out.extend_from_slice(&(EHSIZE as u16).to_le_bytes()); // e_ehsize
    out.extend_from_slice(&(PHENTSIZE as u16).to_le_bytes()); // e_phentsize
    out.extend_from_slice(&1u16.to_le_bytes()); // e_phnum
    out.extend_from_slice(&0u16.to_le_bytes()); // e_shentsize
    out.extend_from_slice(&0u16.to_le_bytes()); // e_shnum
    out.extend_from_slice(&0u16.to_le_bytes()); // e_shstrndx

    out.extend_from_slice(&1u32.to_le_bytes()); // p_type = PT_LOAD
    out.extend_from_slice(&7u32.to_le_bytes()); // p_flags = R | W | X
    out.extend_from_slice(&0u64.to_le_bytes()); // p_offset
    out.extend_from_slice(&VADDR.to_le_bytes()); // p_vaddr
    out.extend_from_slice(&VADDR.to_le_bytes()); // p_paddr
    out.extend_from_slice(&filesz.to_le_bytes()); // p_filesz
    out.extend_from_slice(&memsz.to_le_bytes()); // p_memsz (file image + BSS)
    out.extend_from_slice(&0x1000u64.to_le_bytes()); // p_align

    out.extend_from_slice(code);
    debug_assert_eq!(out.len() as u64, filesz);
    out
}
