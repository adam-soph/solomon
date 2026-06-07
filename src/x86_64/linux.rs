//! The `x86_64-unknown-linux` target: raw Linux syscalls and a freestanding
//! static ELF executable.
//!
//! Code generation itself is shared with the other x86-64 targets; see the
//! parent module. This module supplies only the [`OsTarget`] policy — the
//! syscall sequences for exit, page allocation, and the stdout sink — plus the
//! ELF writer.

use std::path::PathBuf;

use super::{Asm, FileOp, OsTarget, R8, R9, R10, RAX, RCX, RDI, RDX, RSI};
use crate::ast::Program;
use crate::codegen::{Codegen, CodegenError};

/// Compiles a HolyC program to a freestanding static ELF for
/// `x86_64-unknown-linux`. There is no libc and no linker; its own `_start`
/// makes raw syscalls.
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

impl LinuxTarget {
    /// Calls `clock_gettime(clockid, &ts)` (nr 228) over the 16-byte `ts` BSS
    /// slot, then folds `tv_sec*1e9 + tv_nsec` into rax.
    fn emit_clock(asm: &mut Asm, ts: i32, clockid: i32) {
        asm.mov_ri(RDI, clockid);
        asm.lea_global(RSI, ts); // &ts
        asm.mov_ri(RAX, 228); // SYS_clock_gettime
        asm.syscall();
        asm.lea_global(RCX, ts);
        asm.load_qword_at(RAX, RCX); // tv_sec
        asm.imul_rax_imm32(1_000_000_000);
        asm.add_ri(RCX, 8);
        asm.load_qword_at(RDX, RCX); // tv_nsec
        asm.add_rr(RAX, RDX); // rax = sec*1e9 + nsec
    }
}

impl OsTarget for LinuxTarget {
    fn emit_unix_ns(&mut self, asm: &mut Asm, scratch: i32) {
        Self::emit_clock(asm, scratch, 0); // CLOCK_REALTIME
    }

    fn emit_mono_ns(&mut self, asm: &mut Asm, scratch: i32) {
        Self::emit_clock(asm, scratch, 1); // CLOCK_MONOTONIC
    }

    fn emit_sleep(&mut self, asm: &mut Asm, ts: i32) {
        // rax = ns -> timespec(ns/1e9, ns%1e9); nanosleep(&ts, NULL) (nr 35).
        asm.mov_ri(RCX, 1_000_000_000);
        asm.div_rcx(); // rax = sec, rdx = nsec
        asm.lea_global(R8, ts);
        asm.store_qword_at(R8, RAX); // tv_sec
        asm.add_ri(R8, 8);
        asm.store_qword_at(R8, RDX); // tv_nsec
        asm.lea_global(RDI, ts); // &ts
        asm.xor_rr(RSI, RSI); // rem = NULL
        asm.mov_ri(RAX, 35); // SYS_nanosleep
        asm.syscall();
    }

    fn emit_exit(&mut self, asm: &mut Asm) {
        // exit(status in rax): mov rdi, rax; mov rax, 60; syscall.
        asm.mov_rdi_rax();
        asm.mov_rax_imm(60);
        asm.syscall();
    }

    fn emit_page_alloc(&mut self, asm: &mut Asm) {
        // mmap(0, rsi, PROT_READ|WRITE, MAP_PRIVATE|ANON, -1, 0), syscall 9.
        // Returns the base in rax. rsi (the length) is preserved across the syscall.
        asm.mov_ri(RDI, 0);
        asm.mov_ri(RDX, 3);
        asm.mov_ri(R10, 0x22);
        asm.mov_ri(R8, -1);
        asm.mov_ri(R9, 0);
        asm.mov_ri(RAX, 9);
        asm.syscall();
    }

    fn emit_std_write(&mut self, asm: &mut Asm) {
        // write(fd, buf, n): rdi=fd, rsi=buf, rdx=n are already in the syscall
        // registers, since those coincide with the System V arg registers. The
        // syscall number is 1. It leaves the byte count (or -errno) in rax. This
        // is a single write, matching `Write`.
        asm.emit(&[0xB8, 1, 0, 0, 0]); // mov eax, 1 (SYS_write)
        asm.syscall();
    }

    fn emit_fileop(&mut self, asm: &mut Asm, op: FileOp) {
        // The fd args are already in rdi/rsi/rdx, since the System V registers
        // coincide with the syscall registers. The `io.hc` open flags are already
        // Linux's, so each op is a plain syscall: open 2, read 0, write 1, close
        // 3, lseek 8. The result is left in rax.
        let nr = match op {
            FileOp::Read => 0,
            FileOp::Write => 1,
            FileOp::Open => 2,
            FileOp::Close => 3,
            FileOp::LSeek => 8,
        };
        asm.mov_ri(RAX, nr);
        asm.syscall();
    }

    fn emit_capture_args(&mut self, asm: &mut Asm, argc_off: i32, argv_off: i32) {
        // The Linux ELF entry receives `[rsp] = argc`, `[rsp+8] = argv[0]`, and so
        // on. After the prologue's `push rbp; mov rbp, rsp`, `rbp` is the entry
        // rsp − 8, so argc is at `[rbp+8]` and the argv array begins at `rbp+16`.
        asm.emit(&[0x48, 0x8B, 0x45, 0x08]); // mov rax, [rbp+8]   (argc)
        asm.lea_global(RCX, argc_off);
        asm.store_qword_at(RCX, RAX); // argc slot = rax
        asm.emit(&[0x48, 0x8D, 0x45, 0x10]); // lea rax, [rbp+16]  (&argv[0])
        asm.lea_global(RCX, argv_off);
        asm.store_qword_at(RCX, RAX); // argv slot = &argv[0]
    }

    fn emit_capture_env(&mut self, asm: &mut Asm, envp_off: i32) {
        // The env array follows argv's NULL terminator on the initial stack:
        // `&envp[0] = &argv[0] + (argc+1)*8 = rbp + 16 + (argc+1)*8 = rbp + 24 + argc*8`.
        asm.emit(&[0x48, 0x8B, 0x45, 0x08]); // mov rax, [rbp+8]          (argc)
        asm.emit(&[0x48, 0x8D, 0x44, 0xC5, 0x18]); // lea rax, [rbp+rax*8+24] (&envp[0])
        asm.lea_global(RCX, envp_off);
        asm.store_qword_at(RCX, RAX); // envp slot = &envp[0]
    }

    fn wrap(&mut self, asm: Asm, bss: u64) -> Result<Vec<u8>, CodegenError> {
        // Linux has no imports, so finish with an empty import region. This is
        // byte-identical to the plain `[code | strings | bss]` layout. Then wrap
        // it in an ELF.
        let blob = asm.finish(&[], &[])?;
        Ok(write_elf(&blob, bss))
    }
}

// ---- ELF executable writer ----

const VADDR: u64 = 0x40_0000;
const EHSIZE: u64 = 64;
const PHENTSIZE: u64 = 56;

/// Wraps `code` in a minimal static ELF64 executable. The layout is an ELF
/// header plus one PT_LOAD segment (R+W+X) covering the whole file, followed by a
/// trailing zero-filled BSS region of `bss` bytes (globals and print scratch).
/// The entry point is the first code byte.
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
