//! The `aarch64-apple-darwin` OS/container policy: the Mach-O relocatable-object
//! writer and the `cc` link step.
//!
//! This is the only Darwin-specific code. The AArch64 instruction encoding lives
//! in the `asm` module and the code generation in the parent, both OS-agnostic.
//! The encoder hands up a [`CodeImage`] of machine code plus symbolic
//! `SymRef`/`RelKind` relocations. This module lowers those to Mach-O relocation
//! numbers and packages them into an object file.

use std::path::Path;
use std::process::Command;

use super::ArmTarget;
use super::asm::{CodeImage, RelKind, SymRef};
use crate::codegen::CodegenError;

/// The Darwin object/link policy: a Mach-O relocatable object linked with `cc`.
pub(super) struct Darwin;

impl ArmTarget for Darwin {
    fn write_object(
        &self,
        image: &CodeImage,
        defined: &[(String, u64)],
        commons: &[(String, u64, u32)],
        ndefined: u32,
    ) -> Vec<u8> {
        // External (libc) symbols, in first-reference order. They are placed in
        // the symbol table after the defined symbols and common globals, so each
        // gets index `ndefined + commons.len() + position`.
        let mut externs: Vec<&'static str> = Vec::new();
        for (_, sym, _) in &image.relocs {
            if let SymRef::Extern(name) = sym {
                if !externs.contains(name) {
                    externs.push(name);
                }
            }
        }
        let extern_base = ndefined + commons.len() as u32;
        let relocs: Vec<(u32, u32, u32, bool)> = image
            .relocs
            .iter()
            .map(|(addr, sym, kind)| {
                let s = match sym {
                    SymRef::Extern(name) => {
                        extern_base + externs.iter().position(|e| e == name).unwrap() as u32
                    }
                    SymRef::Sym(i) => *i,
                };
                let (ty, pcrel) = match kind {
                    RelKind::Branch26 => (RELOC_BRANCH26, true),
                    RelKind::Page21 => (RELOC_PAGE21, true),
                    RelKind::PageOff12 => (RELOC_PAGEOFF12, false),
                };
                (*addr, s, ty, pcrel)
            })
            .collect();
        write_macho_object(&image.text, defined, commons, &externs, &relocs)
    }

    /// Links with the system `cc`. This is the only place this backend shells out.
    fn link(&self, obj: &Path, out: &Path) -> Result<(), CodegenError> {
        let status = Command::new("cc")
            .arg(obj)
            .arg("-o")
            .arg(out)
            .status()
            .map_err(|e| CodegenError::new(format!("failed to invoke linker `cc`: {e}"), None))?;
        if !status.success() {
            return Err(CodegenError::new(
                format!("linker `cc` failed with status {status}"),
                None,
            ));
        }
        Ok(())
    }

    fn variadic_in_registers(&self) -> bool {
        false // Apple's ARM64 ABI passes all variadic args on the stack
    }
}

const RELOC_BRANCH26: u32 = 2;
const RELOC_PAGE21: u32 = 3;
const RELOC_PAGEOFF12: u32 = 4;

/// Builds the Mach-O object. Symbols are laid out in three groups, matching the
/// indices the relocations were built with: defined symbols first (`_main` plus
/// functions, in `__text`), then common globals, then the undefined externals
/// (libc functions). Globals are *common* symbols, with `n_value` set to the
/// size, so the linker allocates their storage; no data section is needed.
fn write_macho_object(
    text: &[u8],
    defined: &[(String, u64)],
    commons: &[(String, u64, u32)],
    externs: &[&str],
    relocs: &[(u32, u32, u32, bool)],
) -> Vec<u8> {
    let mut strtab = vec![0u8];
    let strx = |s: &mut Vec<u8>, name: &str| -> u32 {
        let at = s.len() as u32;
        s.extend_from_slice(name.as_bytes());
        s.push(0);
        at
    };
    let defined_strx: Vec<u32> = defined.iter().map(|(n, _)| strx(&mut strtab, n)).collect();
    let common_strx: Vec<u32> = commons
        .iter()
        .map(|(n, _, _)| strx(&mut strtab, n))
        .collect();
    let extern_strx: Vec<u32> = externs.iter().map(|n| strx(&mut strtab, n)).collect();

    let nsyms = defined.len() as u32 + commons.len() as u32 + externs.len() as u32;
    let nundef = commons.len() as u32 + externs.len() as u32;

    const HEADER: usize = 32;
    const SEG_CMD: usize = 72 + 80;
    const SYMTAB_CMD: usize = 24;
    const DYSYMTAB_CMD: usize = 80;
    const BUILD_CMD: usize = 24;
    let sizeofcmds = SEG_CMD + SYMTAB_CMD + DYSYMTAB_CMD + BUILD_CMD;

    let code_off = HEADER + sizeofcmds;
    let reloc_off = align8(code_off + text.len());
    let nreloc = relocs.len();
    let sym_off = align8(reloc_off + nreloc * 8);
    let str_off = sym_off + (nsyms as usize) * 16;

    let mut b = Vec::new();

    put_u32(&mut b, 0xFEED_FACF);
    put_u32(&mut b, 0x0100_000C);
    put_u32(&mut b, 0x0000_0000);
    put_u32(&mut b, 1);
    put_u32(&mut b, 4);
    put_u32(&mut b, sizeofcmds as u32);
    put_u32(&mut b, 0);
    put_u32(&mut b, 0);

    put_u32(&mut b, 0x19);
    put_u32(&mut b, SEG_CMD as u32);
    put_name16(&mut b, "");
    put_u64(&mut b, 0);
    put_u64(&mut b, text.len() as u64);
    put_u64(&mut b, code_off as u64);
    put_u64(&mut b, text.len() as u64);
    put_u32(&mut b, 7);
    put_u32(&mut b, 7);
    put_u32(&mut b, 1);
    put_u32(&mut b, 0);
    put_name16(&mut b, "__text");
    put_name16(&mut b, "__TEXT");
    put_u64(&mut b, 0);
    put_u64(&mut b, text.len() as u64);
    put_u32(&mut b, code_off as u32);
    put_u32(&mut b, 2);
    put_u32(&mut b, reloc_off as u32);
    put_u32(&mut b, nreloc as u32);
    put_u32(&mut b, 0x0000_0400);
    put_u32(&mut b, 0);
    put_u32(&mut b, 0);
    put_u32(&mut b, 0);

    put_u32(&mut b, 0x02);
    put_u32(&mut b, SYMTAB_CMD as u32);
    put_u32(&mut b, sym_off as u32);
    put_u32(&mut b, nsyms);
    put_u32(&mut b, str_off as u32);
    put_u32(&mut b, strtab.len() as u32);

    put_u32(&mut b, 0x0B);
    put_u32(&mut b, DYSYMTAB_CMD as u32);
    put_u32(&mut b, 0); // ilocalsym
    put_u32(&mut b, 0); // nlocalsym
    put_u32(&mut b, 0); // iextdefsym
    put_u32(&mut b, defined.len() as u32); // nextdefsym
    put_u32(&mut b, defined.len() as u32); // iundefsym
    put_u32(&mut b, nundef); // nundefsym
    for _ in 0..12 {
        put_u32(&mut b, 0);
    }

    put_u32(&mut b, 0x32);
    put_u32(&mut b, BUILD_CMD as u32);
    put_u32(&mut b, 1);
    put_u32(&mut b, 0x000B_0000);
    put_u32(&mut b, 0x000B_0000);
    put_u32(&mut b, 0);

    debug_assert_eq!(b.len(), code_off);
    b.extend_from_slice(text);

    while b.len() < reloc_off {
        b.push(0);
    }
    for &(addr, sym, rtype, pcrel) in relocs {
        put_u32(&mut b, addr);
        let packed = (sym & 0x00FF_FFFF)
            | ((pcrel as u32) << 24)
            | (2 << 25) // r_length = 2
            | (1 << 27) // r_extern = 1
            | (rtype << 28);
        put_u32(&mut b, packed);
    }

    while b.len() < sym_off {
        b.push(0);
    }
    for (i, (_, value)) in defined.iter().enumerate() {
        put_u32(&mut b, defined_strx[i]);
        b.push(0x0F); // N_SECT | N_EXT
        b.push(1);
        put_u16(&mut b, 0);
        put_u64(&mut b, *value);
    }
    for (i, (_, size, align_log2)) in commons.iter().enumerate() {
        put_u32(&mut b, common_strx[i]);
        b.push(0x01); // N_UNDF | N_EXT  (n_value=size => common/tentative)
        b.push(0);
        put_u16(&mut b, ((align_log2 & 0xF) << 8) as u16); // common alignment
        put_u64(&mut b, *size);
    }
    for &sx in &extern_strx {
        put_u32(&mut b, sx);
        b.push(0x01); // N_UNDF | N_EXT
        b.push(0);
        put_u16(&mut b, 0);
        put_u64(&mut b, 0);
    }

    debug_assert_eq!(b.len(), str_off);
    b.extend_from_slice(&strtab);
    b
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
fn put_name16(b: &mut Vec<u8>, name: &str) {
    let mut field = [0u8; 16];
    let bytes = name.as_bytes();
    let n = bytes.len().min(16);
    field[..n].copy_from_slice(&bytes[..n]);
    b.extend_from_slice(&field);
}
