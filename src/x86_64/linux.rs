//! The `x86_64-unknown-linux` target: raw Linux syscalls and a freestanding
//! static ELF executable. The code generation itself is shared (see the parent
//! module); this module supplies only the [`OsTarget`] policy — the syscall
//! sequences for exit / page allocation / the stdout sink — and the ELF writer.

use std::path::PathBuf;

use super::{Asm, OsTarget, R8, R9, R10, RAX, RCX, RDI, RDX, RSI};
use crate::ast::Program;
use crate::codegen::{Codegen, CodegenError};

/// Compiles a HolyC program to a freestanding static ELF for `x86_64-unknown-linux`
/// (no libc, no linker — its own `_start` makes raw syscalls).
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
    fn has_posix_clock(&self) -> bool {
        true
    }

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
        // write(1, rsi, rdx), looping until the whole buffer is written: `write`
        // may return a short count, and `-EINTR` (-4) means retry. rsi=buf, rdx=remaining.
        //
        // The raw `syscall` instruction clobbers rcx and r11, but the format
        // routines treat OutWrite as an ordinary call and keep working state there
        // — fmt_str holds the string length in r11 across the pad write, so a
        // right-justified `%s` would otherwise read a garbage length and dump
        // memory (real hardware clobbers r11; qemu does not, which hid this). Save
        // and restore the syscall-clobbered registers so OutWrite preserves them.
        asm.emit(&[0x51]); // push rcx
        asm.emit(&[0x41, 0x53]); // push r11
        let wloop = asm.new_label();
        let advance = asm.new_label();
        let wdone = asm.new_label();
        asm.place(wloop);
        asm.test_rr(RDX, RDX);
        asm.je(wdone); // nothing left
        asm.emit(&[0xB8, 1, 0, 0, 0]); // mov eax, 1 (SYS_write)
        asm.emit(&[0xBF, 1, 0, 0, 0]); // mov edi, 1 (stdout)
        asm.syscall(); // rax = bytes written (or -errno)
        asm.test_rr(RAX, RAX);
        asm.jg(advance); // wrote >0 bytes
        asm.cmp_ri(RAX, -4); // -EINTR?
        asm.je(wloop); // EINTR: retry same buf/len
        asm.jmp(wdone); // other error / 0: give up
        asm.place(advance);
        asm.add_rr(RSI, RAX); // buf += written
        asm.sub_rr(RDX, RAX); // remaining -= written
        asm.jmp(wloop);
        asm.place(wdone);
        asm.emit(&[0x41, 0x5B]); // pop r11
        asm.emit(&[0x59]); // pop rcx
    }

    fn emit_capture_args(&mut self, asm: &mut Asm, argc_off: i32, argv_off: i32) {
        // The Linux ELF entry receives `[rsp] = argc`, `[rsp+8] = argv[0]`, …
        // After the prologue's `push rbp; mov rbp, rsp`, `rbp` = the entry rsp − 8,
        // so argc is at `[rbp+8]` and the argv array begins at `rbp+16`.
        asm.emit(&[0x48, 0x8B, 0x45, 0x08]); // mov rax, [rbp+8]   (argc)
        asm.lea_global(RCX, argc_off);
        asm.store_qword_at(RCX, RAX); // argc slot = rax
        asm.emit(&[0x48, 0x8D, 0x45, 0x10]); // lea rax, [rbp+16]  (&argv[0])
        asm.lea_global(RCX, argv_off);
        asm.store_qword_at(RCX, RAX); // argv slot = &argv[0]
    }

    fn wrap(&mut self, asm: Asm, bss: u64) -> Result<Vec<u8>, CodegenError> {
        // No imports on Linux: finish with an empty import region (byte-identical
        // to the former `[code | strings | bss]` layout), then wrap in an ELF.
        let blob = asm.finish(&[], &[])?;
        Ok(write_elf(&blob, bss))
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
