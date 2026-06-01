//! The `x86_64-pc-windows` target: `kernel32` imports and a self-contained PE
//! executable.
//!
//! The x86-64 code generation is shared with the Linux target through the parent
//! module's [`OsTarget`] seam; this module supplies only the Windows policy. The
//! three OS seams lower to `kernel32` calls through the import address table
//! (`ExitProcess`, `VirtualAlloc`, `GetStdHandle`+`WriteFile`), marshaled into the
//! Microsoft x64 ABI (args in `rcx/rdx/r8/r9`, a 32-byte shadow area, 16-byte
//! alignment at the call). The container is a hand-built PE — no linker, like the
//! Linux ELF — with a `kernel32.dll` import directory. Code, strings, the import
//! table, and the BSS share one R+W+X section that maps 1:1 with the file, so the
//! encoder's RIP-relative `target - (pos+4)` references resolve unchanged.

use std::path::PathBuf;

use super::{Asm, OsTarget};
use crate::ast::Program;
use crate::codegen::{Codegen, CodegenError};

const IMAGE_BASE: u64 = 0x1_4000_0000;
const SECTION_RVA: u32 = 0x1000;
const SECTION_ALIGN: u32 = 0x1000;
const FILE_ALIGN: u32 = 0x200;
/// DOS(64) + PE sig(4) + COFF(20) + optional header(240) + one section(40) = 368,
/// rounded up to the file alignment.
const SIZE_OF_HEADERS: u32 = 0x200;

/// Compiles a HolyC program to a self-contained PE executable for `x86_64-pc-windows`.
pub struct X64Windows {
    out_path: PathBuf,
}

impl X64Windows {
    pub fn new(out_path: impl Into<PathBuf>) -> Self {
        X64Windows {
            out_path: out_path.into(),
        }
    }
}

impl Codegen for X64Windows {
    fn name(&self) -> &'static str {
        "x86_64-pc-windows"
    }

    fn run(&mut self, program: &Program) -> Result<(), CodegenError> {
        let pe = super::compile(program, Box::new(WindowsTarget::new()))?;
        std::fs::write(&self.out_path, &pe)
            .map_err(|e| CodegenError::new(format!("cannot write PE executable: {e}"), None))
    }
}

/// The Windows OS policy: `kernel32` import shims and a self-contained PE.
struct WindowsTarget {
    /// `kernel32` functions referenced, in import-table (and IAT) order. The
    /// index of each is the slot `call_extern` records for the call site.
    externs: Vec<&'static str>,
}

impl WindowsTarget {
    fn new() -> Self {
        WindowsTarget {
            externs: Vec::new(),
        }
    }

    /// The import slot for `name`, assigning a new one on first reference.
    fn extern_idx(&mut self, name: &'static str) -> usize {
        if let Some(i) = self.externs.iter().position(|&e| e == name) {
            return i;
        }
        self.externs.push(name);
        self.externs.len() - 1
    }
}

impl OsTarget for WindowsTarget {
    fn emit_exit(&mut self, asm: &mut Asm) {
        // ExitProcess(uExitCode = eax). rsp is 16-aligned here (inside the entry
        // frame), so a 32-byte shadow area keeps it aligned at the call; the call
        // does not return.
        let i = self.extern_idx("ExitProcess");
        asm.emit(&[0x89, 0xC1]); // mov ecx, eax
        asm.emit(&[0x48, 0x83, 0xEC, 0x20]); // sub rsp, 32 (shadow space)
        asm.call_extern(i); // call [ExitProcess]
    }

    fn emit_page_alloc(&mut self, asm: &mut Asm) {
        // VirtualAlloc(NULL, rsi, MEM_COMMIT|MEM_RESERVE, PAGE_READWRITE) -> base.
        // rsi (size) and rbx are non-volatile on Win64, so both survive the call.
        let i = self.extern_idx("VirtualAlloc");
        asm.emit(&[0x31, 0xC9]); // xor ecx, ecx            (lpAddress = NULL)
        asm.emit(&[0x48, 0x89, 0xF2]); // mov rdx, rsi      (dwSize)
        asm.emit(&[0x41, 0xB8, 0x00, 0x30, 0x00, 0x00]); // mov r8d, 0x3000
        asm.emit(&[0x41, 0xB9, 0x04, 0x00, 0x00, 0x00]); // mov r9d, 4 (PAGE_READWRITE)
        asm.emit(&[0x48, 0x83, 0xEC, 0x20]); // sub rsp, 32 (shadow)
        asm.call_extern(i); // call [VirtualAlloc]   -> rax = base
        asm.emit(&[0x48, 0x83, 0xC4, 0x20]); // add rsp, 32
    }

    fn emit_write_stdout(&mut self, asm: &mut Asm) {
        // WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), rsi, rdx, &written, NULL).
        // A 72-byte frame (≡ 8 mod 16, so rsp is 16-aligned at each call) holds the
        // shadow area, the 5th stack arg, a scratch `written` DWORD, and the saved
        // buffer/length across the GetStdHandle call (which clobbers volatile rdx).
        let gsh = self.extern_idx("GetStdHandle");
        let wf = self.extern_idx("WriteFile");
        asm.emit(&[0x48, 0x83, 0xEC, 0x48]); // sub rsp, 72
        asm.emit(&[0x48, 0x89, 0x74, 0x24, 0x30]); // mov [rsp+48], rsi  (save buf)
        asm.emit(&[0x48, 0x89, 0x54, 0x24, 0x38]); // mov [rsp+56], rdx  (save len)
        asm.emit(&[0xB9, 0xF5, 0xFF, 0xFF, 0xFF]); // mov ecx, -11 (STD_OUTPUT_HANDLE)
        asm.call_extern(gsh); // call [GetStdHandle] -> rax = handle
        asm.emit(&[0x48, 0x89, 0xC1]); // mov rcx, rax          (hFile)
        asm.emit(&[0x48, 0x8B, 0x54, 0x24, 0x30]); // mov rdx, [rsp+48]  (lpBuffer)
        asm.emit(&[0x4C, 0x8B, 0x44, 0x24, 0x38]); // mov r8, [rsp+56]   (nBytes)
        asm.emit(&[0x4C, 0x8D, 0x4C, 0x24, 0x28]); // lea r9, [rsp+40]   (&written)
        asm.emit(&[0x48, 0xC7, 0x44, 0x24, 0x20, 0x00, 0x00, 0x00, 0x00]); // mov qword [rsp+32], 0
        asm.call_extern(wf); // call [WriteFile]
        asm.emit(&[0x48, 0x83, 0xC4, 0x48]); // add rsp, 72
    }

    fn wrap(&mut self, asm: Asm, bss: u64) -> Result<Vec<u8>, CodegenError> {
        let n = self.externs.len();
        // The import region is appended after the code and strings; compute where
        // it will land so its internal RVAs (and the IAT) are correct.
        let import_base = SECTION_RVA as usize + asm.code_len() + asm.strings_total();

        // Layout within the import region: import directory table (one descriptor
        // + a null terminator), the import lookup table, the import address table
        // (what the call sites target), then the hint/name table and DLL name.
        let idt_size = 40usize; // 2 × IMAGE_IMPORT_DESCRIPTOR (kernel32 + null)
        let thunks = (n + 1) * 8; // n names + a null terminator, 8 bytes each (PE32+)
        let ilt_off = idt_size;
        let iat_off = ilt_off + thunks;
        let hn_off = iat_off + thunks;

        // Hint/name entries (2-byte hint + name + NUL, padded to even), recording
        // each function's RVA for the lookup/address tables.
        let mut hn = Vec::new();
        let mut name_rva = Vec::with_capacity(n);
        let mut cur = hn_off;
        for &name in &self.externs {
            name_rva.push((import_base + cur) as u32);
            put16(&mut hn, 0); // hint
            hn.extend_from_slice(name.as_bytes());
            hn.push(0);
            let mut len = 2 + name.len() + 1;
            if len % 2 != 0 {
                hn.push(0);
                len += 1;
            }
            cur += len;
        }
        let dll_off = hn_off + hn.len();

        let idt_rva = import_base as u32;
        let ilt_rva = (import_base + ilt_off) as u32;
        let iat_rva = (import_base + iat_off) as u32;
        let dll_rva = (import_base + dll_off) as u32;

        let mut import = Vec::new();
        // IMAGE_IMPORT_DESCRIPTOR for kernel32.dll, then the null terminator.
        put32(&mut import, ilt_rva); // OriginalFirstThunk (ILT)
        put32(&mut import, 0); // TimeDateStamp
        put32(&mut import, 0); // ForwarderChain
        put32(&mut import, dll_rva); // Name
        put32(&mut import, iat_rva); // FirstThunk (IAT)
        import.extend_from_slice(&[0u8; 20]); // null descriptor
        for &rva in &name_rva {
            put64(&mut import, rva as u64); // ILT entry (by name)
        }
        put64(&mut import, 0);
        for &rva in &name_rva {
            put64(&mut import, rva as u64); // IAT entry (loader fills in the address)
        }
        put64(&mut import, 0);
        import.extend_from_slice(&hn);
        import.extend_from_slice(b"kernel32.dll\0");

        // Each call_extern slot `i` targets IAT entry `i`.
        let iat_offsets: Vec<usize> = (0..n).map(|i| iat_off + i * 8).collect();

        let blob = asm.finish(&import, &iat_offsets)?;
        Ok(build_pe(
            &blob,
            bss,
            idt_rva,
            idt_size as u32,
            iat_rva,
            (thunks) as u32,
        ))
    }
}

/// Wrap the finished image (`[code | strings | import]`, with `bss` zero bytes
/// following it in memory) in a minimal PE32+ executable: one R+W+X section that
/// maps the image 1:1, plus an import directory pointing at `kernel32.dll`.
fn build_pe(
    blob: &[u8],
    bss: u64,
    idt_rva: u32,
    idt_size: u32,
    iat_rva: u32,
    iat_size: u32,
) -> Vec<u8> {
    let raw_size = align(blob.len() as u32, FILE_ALIGN);
    let virt_size = blob.len() as u32 + bss as u32;
    let size_of_image = align(SECTION_RVA + virt_size, SECTION_ALIGN);

    let mut h = Vec::new();
    // DOS header: just "MZ" and e_lfanew at 0x3C (no DOS stub program).
    h.extend_from_slice(b"MZ");
    h.resize(0x3C, 0);
    put32(&mut h, 0x40); // e_lfanew -> PE signature right after the DOS header

    h.extend_from_slice(b"PE\0\0");
    // COFF header.
    put16(&mut h, 0x8664); // Machine = IMAGE_FILE_MACHINE_AMD64
    put16(&mut h, 1); // NumberOfSections
    put32(&mut h, 0); // TimeDateStamp
    put32(&mut h, 0); // PointerToSymbolTable
    put32(&mut h, 0); // NumberOfSymbols
    put16(&mut h, 240); // SizeOfOptionalHeader
    put16(&mut h, 0x0022); // Characteristics: EXECUTABLE_IMAGE | LARGE_ADDRESS_AWARE

    // Optional header (PE32+).
    put16(&mut h, 0x020B); // Magic = PE32+
    h.push(0); // MajorLinkerVersion
    h.push(0); // MinorLinkerVersion
    put32(&mut h, raw_size); // SizeOfCode
    put32(&mut h, 0); // SizeOfInitializedData
    put32(&mut h, 0); // SizeOfUninitializedData
    put32(&mut h, SECTION_RVA); // AddressOfEntryPoint (code is at section start)
    put32(&mut h, SECTION_RVA); // BaseOfCode
    put64(&mut h, IMAGE_BASE); // ImageBase
    put32(&mut h, SECTION_ALIGN); // SectionAlignment
    put32(&mut h, FILE_ALIGN); // FileAlignment
    put16(&mut h, 6); // MajorOperatingSystemVersion
    put16(&mut h, 0); // MinorOperatingSystemVersion
    put16(&mut h, 0); // MajorImageVersion
    put16(&mut h, 0); // MinorImageVersion
    put16(&mut h, 6); // MajorSubsystemVersion
    put16(&mut h, 0); // MinorSubsystemVersion
    put32(&mut h, 0); // Win32VersionValue
    put32(&mut h, size_of_image); // SizeOfImage
    put32(&mut h, SIZE_OF_HEADERS); // SizeOfHeaders
    put32(&mut h, 0); // CheckSum
    put16(&mut h, 3); // Subsystem = IMAGE_SUBSYSTEM_WINDOWS_CUI (console)
    put16(&mut h, 0); // DllCharacteristics (no DYNAMIC_BASE: load at ImageBase)
    put64(&mut h, 0x10_0000); // SizeOfStackReserve
    put64(&mut h, 0x1000); // SizeOfStackCommit
    put64(&mut h, 0x10_0000); // SizeOfHeapReserve
    put64(&mut h, 0x1000); // SizeOfHeapCommit
    put32(&mut h, 0); // LoaderFlags
    put32(&mut h, 16); // NumberOfRvaAndSizes

    // 16 data directories: only Import (1) and IAT (12) are populated.
    put32(&mut h, 0); // 0 Export
    put32(&mut h, 0);
    put32(&mut h, idt_rva); // 1 Import
    put32(&mut h, idt_size);
    for _ in 2..12 {
        put32(&mut h, 0);
        put32(&mut h, 0);
    }
    put32(&mut h, iat_rva); // 12 IAT
    put32(&mut h, iat_size);
    for _ in 13..16 {
        put32(&mut h, 0);
        put32(&mut h, 0);
    }

    // Section table (one R+W+X section mapping the whole image).
    let mut name = [0u8; 8];
    name[..5].copy_from_slice(b".text");
    h.extend_from_slice(&name);
    put32(&mut h, virt_size); // VirtualSize (includes the BSS tail)
    put32(&mut h, SECTION_RVA); // VirtualAddress
    put32(&mut h, raw_size); // SizeOfRawData (file bytes, no BSS)
    put32(&mut h, SIZE_OF_HEADERS); // PointerToRawData
    put32(&mut h, 0); // PointerToRelocations
    put32(&mut h, 0); // PointerToLinenumbers
    put16(&mut h, 0); // NumberOfRelocations
    put16(&mut h, 0); // NumberOfLinenumbers
    put32(&mut h, 0xE000_0020); // CODE | EXECUTE | READ | WRITE

    h.resize(SIZE_OF_HEADERS as usize, 0); // pad headers to the file alignment
    h.extend_from_slice(blob);
    h.resize(SIZE_OF_HEADERS as usize + raw_size as usize, 0); // pad section to raw size
    h
}

fn align(n: u32, to: u32) -> u32 {
    n.div_ceil(to) * to
}
fn put16(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_le_bytes());
}
