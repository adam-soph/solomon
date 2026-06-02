//! The `aarch64-unknown-linux-gnu` target: a glibc-linked ELF executable.
//!
//! The AArch64 instruction encoding and the code generation are shared with the
//! Darwin backend (the [`asm`](super::asm) module and the parent's `compile`
//! driver). This module supplies only the Linux object/link policy: an **ELF
//! relocatable-object** container — with AArch64 ELF relocation types and bare
//! symbol names (no Mach-O leading underscore) — linked with an AArch64 Linux
//! `gcc` against glibc. The emitted `main` is called by glibc's `_start` with
//! `argc`/`argv` in `x0`/`x1` (so `ArgC`/`ArgV` capture works unchanged) and its
//! return value becomes the process exit code, exactly as on Darwin via `cc`.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::ArmTarget;
use super::asm::{CodeImage, RelKind, SymRef};
use crate::ast::Program;
use crate::codegen::{Codegen, CodegenError};

/// Compiles a HolyC program for `aarch64-unknown-linux-{gnu,musl}`. The code
/// generation and ELF object are identical for both libc flavors; only the link
/// differs — glibc (dynamic) for `-gnu`, musl (static) for `-musl`.
pub struct Arm64Linux {
    out_path: PathBuf,
    target: Linux,
}

impl Arm64Linux {
    /// `aarch64-unknown-linux-gnu` — dynamically linked against glibc.
    pub fn new(out_path: impl Into<PathBuf>) -> Self {
        Arm64Linux {
            out_path: out_path.into(),
            target: Linux::GNU,
        }
    }

    /// `aarch64-unknown-linux-musl` — statically linked against musl.
    pub fn new_musl(out_path: impl Into<PathBuf>) -> Self {
        Arm64Linux {
            out_path: out_path.into(),
            target: Linux::MUSL,
        }
    }

    /// `aarch64-unknown-linux` **freestanding** — a self-contained static ELF with
    /// its own `_start` and raw syscalls, no libc and no linker (the AArch64
    /// analogue of the freestanding `x86_64-unknown-linux` backend).
    pub fn new_freestanding(out_path: impl Into<PathBuf>) -> Self {
        Arm64Linux {
            out_path: out_path.into(),
            target: Linux::FREESTANDING,
        }
    }

    /// Emit the ELF relocatable object for `program` as raw bytes (no link).
    /// Exposed so structural tests can byte-check the object on any host.
    pub fn object(&self, program: &Program) -> Result<Vec<u8>, CodegenError> {
        super::compile(program, &self.target)
    }
}

impl Codegen for Arm64Linux {
    fn name(&self) -> &'static str {
        self.target.triple
    }

    fn run(&mut self, program: &Program) -> Result<(), CodegenError> {
        super::build(program, &self.out_path, &self.target)
    }
}

/// The Linux object/link policy: an AArch64 ELF relocatable object linked against
/// a libc. The two flavors share the object writer and differ only in the linker.
#[derive(Clone, Copy)]
struct Linux {
    triple: &'static str,
    default_cc: &'static str,
    static_link: bool,
    /// Freestanding: emit a self-contained executable, no libc, no linker.
    freestanding: bool,
}

impl Linux {
    const GNU: Linux = Linux {
        triple: "aarch64-unknown-linux-gnu",
        default_cc: "aarch64-linux-gnu-gcc",
        static_link: false,
        freestanding: false,
    };
    const MUSL: Linux = Linux {
        triple: "aarch64-unknown-linux-musl",
        default_cc: "aarch64-linux-musl-gcc",
        static_link: true,
        freestanding: false,
    };
    const FREESTANDING: Linux = Linux {
        triple: "aarch64-unknown-linux",
        default_cc: "",
        static_link: true,
        freestanding: true,
    };
}

// AArch64 ELF relocation types (the ones the encoder produces).
const R_AARCH64_ADR_PREL_PG_HI21: u32 = 275;
const R_AARCH64_ADD_ABS_LO12_NC: u32 = 277;
const R_AARCH64_CALL26: u32 = 283;

impl ArmTarget for Linux {
    fn write_object(
        &self,
        image: &CodeImage,
        defined: &[(String, u64)],
        commons: &[(String, u64, u32)],
        ndefined: u32,
    ) -> Vec<u8> {
        // External (libc) symbols, in first-reference order — same layout as the
        // Mach-O writer, so the relocation symbol indices match.
        let mut externs: Vec<&'static str> = Vec::new();
        for (_, sym, _) in &image.relocs {
            if let SymRef::Extern(name) = sym {
                if !externs.contains(name) {
                    externs.push(name);
                }
            }
        }
        let extern_base = ndefined + commons.len() as u32;
        // (r_offset, ELF symbol index, r_type). ELF symbol tables start with a
        // null entry, so every defined/common/extern symbol shifts up by one.
        let relocs: Vec<(u64, u32, u32)> = image
            .relocs
            .iter()
            .map(|(addr, sym, kind)| {
                let s = match sym {
                    SymRef::Extern(name) => {
                        extern_base + externs.iter().position(|e| e == name).unwrap() as u32
                    }
                    SymRef::Sym(i) => *i,
                };
                let ty = match kind {
                    RelKind::Branch26 => R_AARCH64_CALL26,
                    RelKind::Page21 => R_AARCH64_ADR_PREL_PG_HI21,
                    RelKind::PageOff12 => R_AARCH64_ADD_ABS_LO12_NC,
                };
                (*addr as u64, s + 1, ty)
            })
            .collect();
        write_elf_object(&image.text, defined, commons, &externs, &relocs)
    }

    fn link(&self, obj: &Path, out: &Path) -> Result<(), CodegenError> {
        // Default to the flavor's cross-toolchain name; override with HOLYC_CC
        // (e.g. on a native host where the compiler is just `gcc`).
        let cc = std::env::var("HOLYC_CC").unwrap_or_else(|_| self.default_cc.to_string());
        let mut cmd = Command::new(&cc);
        cmd.arg(obj).arg("-o").arg(out);
        cmd.arg("-lm"); // libm: `sqrt` etc. are separate from libc on Linux
        if self.static_link {
            cmd.arg("-static"); // musl: a self-contained binary
        }
        let status = cmd.status().map_err(|e| {
            CodegenError::new(
                format!("failed to invoke linker `{cc}`: {e} (set HOLYC_CC to your aarch64 Linux compiler)"),
                None,
            )
        })?;
        if !status.success() {
            return Err(CodegenError::new(
                format!("linker `{cc}` failed with status {status}"),
                None,
            ));
        }
        Ok(())
    }

    fn variadic_in_registers(&self) -> bool {
        true // standard AAPCS64 passes variadic args in registers
    }

    fn freestanding(&self) -> bool {
        self.freestanding
    }

    fn write_executable(&self, code: &[u8], bss: u64) -> Vec<u8> {
        write_elf_exec(code, bss)
    }
}

// ---- freestanding ELF executable writer ----

const VADDR: u64 = 0x40_0000;
const EHSIZE: u64 = 64;
const PHENTSIZE: u64 = 56;

/// Wrap freestanding `code` in a minimal static ELF64 executable: an ELF header,
/// one `PT_LOAD` (R+W+X) covering the whole file plus a trailing zero-filled BSS
/// region of `bss` bytes, with the entry at the first code byte (the emitted
/// `_start`). Mirrors the `x86_64-unknown-linux` writer, with `e_machine =
/// EM_AARCH64`.
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

/// Strip the Mach-O-style leading underscore the code generator adds; ELF symbols
/// are bare (`_main` -> `main`, `_printf` -> `printf`).
fn bare(name: &str) -> &str {
    name.strip_prefix('_').unwrap_or(name)
}

/// Build an AArch64 ELF64 relocatable object: an ELF header, then `.text`,
/// `.rela.text`, `.symtab`, `.strtab`, `.shstrtab`, and the section header table.
/// Symbols are laid out null, defined (`.text` funcs), commons (`SHN_COMMON`
/// globals), then undefined externs — matching the relocation indices.
fn write_elf_object(
    text: &[u8],
    defined: &[(String, u64)],
    commons: &[(String, u64, u32)],
    externs: &[&str],
    relocs: &[(u64, u32, u32)],
) -> Vec<u8> {
    // String table for symbol names (starts with a NUL).
    let mut strtab = vec![0u8];
    let mut strx = |s: &str| -> u32 {
        let at = strtab.len() as u32;
        strtab.extend_from_slice(bare(s).as_bytes());
        strtab.push(0);
        at
    };
    let defined_nx: Vec<u32> = defined.iter().map(|(n, _)| strx(n)).collect();
    let common_nx: Vec<u32> = commons.iter().map(|(n, _, _)| strx(n)).collect();
    let extern_nx: Vec<u32> = externs.iter().map(|n| strx(n)).collect();

    let nsyms = 1 + defined.len() + commons.len() + externs.len();

    // Section name string table.
    let mut shstr = vec![0u8];
    let mut shname = |s: &str| -> u32 {
        let at = shstr.len() as u32;
        shstr.extend_from_slice(s.as_bytes());
        shstr.push(0);
        at
    };
    let n_text = shname(".text");
    let n_rela = shname(".rela.text");
    let n_symtab = shname(".symtab");
    let n_strtab = shname(".strtab");
    let n_shstrtab = shname(".shstrtab");

    // File layout.
    let text_off = 64usize;
    let rela_off = align8(text_off + text.len());
    let sym_off = rela_off + relocs.len() * 24;
    let str_off = sym_off + nsyms * 24;
    let shstr_off = str_off + strtab.len();
    let sh_off = align8(shstr_off + shstr.len());

    let mut b = Vec::new();

    // ELF header.
    b.extend_from_slice(&[0x7F, b'E', b'L', b'F', 2, 1, 1, 0]); // magic + class64 + LE + v1 + SysV
    b.extend_from_slice(&[0u8; 8]); // EI_PAD
    put_u16(&mut b, 1); // e_type = ET_REL
    put_u16(&mut b, 183); // e_machine = EM_AARCH64
    put_u32(&mut b, 1); // e_version
    put_u64(&mut b, 0); // e_entry
    put_u64(&mut b, 0); // e_phoff
    put_u64(&mut b, sh_off as u64); // e_shoff
    put_u32(&mut b, 0); // e_flags
    put_u16(&mut b, 64); // e_ehsize
    put_u16(&mut b, 0); // e_phentsize
    put_u16(&mut b, 0); // e_phnum
    put_u16(&mut b, 64); // e_shentsize
    put_u16(&mut b, 6); // e_shnum
    put_u16(&mut b, 5); // e_shstrndx (.shstrtab)

    // .text
    debug_assert_eq!(b.len(), text_off);
    b.extend_from_slice(text);

    // .rela.text
    while b.len() < rela_off {
        b.push(0);
    }
    for &(r_offset, sym, ty) in relocs {
        put_u64(&mut b, r_offset);
        put_u64(&mut b, ((sym as u64) << 32) | ty as u64); // r_info
        put_u64(&mut b, 0); // r_addend (the instruction immediates are zero)
    }

    // .symtab — null, defined (FUNC in .text), commons (SHN_COMMON), externs (UNDEF).
    debug_assert_eq!(b.len(), sym_off);
    put_sym(&mut b, 0, 0, 0, 0, 0); // null symbol
    for (i, (_, value)) in defined.iter().enumerate() {
        put_sym(&mut b, defined_nx[i], 0x12, 1, *value, 0); // GLOBAL|FUNC, shndx=.text
    }
    for (i, (_, size, align_log2)) in commons.iter().enumerate() {
        // A COMMON symbol: st_shndx = SHN_COMMON, st_value = alignment, st_size = size.
        put_sym(
            &mut b,
            common_nx[i],
            0x11,
            0xFFF2,
            1u64 << align_log2,
            *size,
        );
    }
    for (i, _) in externs.iter().enumerate() {
        put_sym(&mut b, extern_nx[i], 0x10, 0, 0, 0); // GLOBAL|NOTYPE, SHN_UNDEF
    }

    // .strtab, .shstrtab
    debug_assert_eq!(b.len(), str_off);
    b.extend_from_slice(&strtab);
    debug_assert_eq!(b.len(), shstr_off);
    b.extend_from_slice(&shstr);

    // Section header table.
    while b.len() < sh_off {
        b.push(0);
    }
    put_shdr(&mut b, 0, 0, 0, 0, 0, 0, 0, 0, 0); // null
    // .text: PROGBITS, ALLOC|EXECINSTR
    put_shdr(
        &mut b,
        n_text,
        1,
        0x6,
        text_off as u64,
        text.len() as u64,
        0,
        0,
        4,
        0,
    );
    // .rela.text: RELA, INFO_LINK; link=.symtab(3), info=.text(1)
    put_shdr(
        &mut b,
        n_rela,
        4,
        0x40,
        rela_off as u64,
        (relocs.len() * 24) as u64,
        3,
        1,
        8,
        24,
    );
    // .symtab: SYMTAB; link=.strtab(4), info=index of first global symbol (1)
    put_shdr(
        &mut b,
        n_symtab,
        2,
        0,
        sym_off as u64,
        (nsyms * 24) as u64,
        4,
        1,
        8,
        24,
    );
    // .strtab
    put_shdr(
        &mut b,
        n_strtab,
        3,
        0,
        str_off as u64,
        strtab.len() as u64,
        0,
        0,
        1,
        0,
    );
    // .shstrtab
    put_shdr(
        &mut b,
        n_shstrtab,
        3,
        0,
        shstr_off as u64,
        shstr.len() as u64,
        0,
        0,
        1,
        0,
    );

    b
}

#[allow(clippy::too_many_arguments)]
fn put_sym(b: &mut Vec<u8>, name: u32, info: u8, shndx: u16, value: u64, size: u64) {
    put_u32(b, name);
    b.push(info);
    b.push(0); // st_other
    put_u16(b, shndx);
    put_u64(b, value);
    put_u64(b, size);
}

#[allow(clippy::too_many_arguments)]
fn put_shdr(
    b: &mut Vec<u8>,
    name: u32,
    sh_type: u32,
    flags: u64,
    offset: u64,
    size: u64,
    link: u32,
    info: u32,
    addralign: u64,
    entsize: u64,
) {
    put_u32(b, name);
    put_u32(b, sh_type);
    put_u64(b, flags);
    put_u64(b, 0); // sh_addr
    put_u64(b, offset);
    put_u64(b, size);
    put_u32(b, link);
    put_u32(b, info);
    put_u64(b, addralign);
    put_u64(b, entsize);
}

fn align8(n: usize) -> usize {
    (n + 7) & !7
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
