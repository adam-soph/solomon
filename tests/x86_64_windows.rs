//! Tests for the x86-64 / PE (Windows) backend.
//!
//! The backend can't be *executed* on the dev host (an Apple-silicon Mac runs the
//! amd64 image under QEMU, where Wine can't run x86 Windows code), so — like the
//! x86-64 Linux structural tests — these instead **byte-scan the emitted PE**:
//! the PE32+ headers, and the exact instruction bytes of each `kernel32` shim,
//! checking that every `call [rip]` resolves through the import address table to
//! the function it should. This pins the generated code without running it.

use std::sync::atomic::{AtomicU32, Ordering};

use solomon::codegen::Codegen;
use solomon::parser::parse;
use solomon::sema::check_program;
use solomon::x86_64::X64Windows;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_out() -> std::path::PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("solomon-win-{}-{id}.exe", std::process::id()))
}

/// Compile `src` to a PE image (written to a temp file, then read back).
fn build_pe(src: &str) -> Vec<u8> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    let errs = check_program(&program);
    assert!(errs.is_empty(), "semantic errors: {errs:?}");
    let out = temp_out();
    X64Windows::new(&out)
        .run(&program)
        .unwrap_or_else(|e| panic!("windows build failed: {e}"));
    let bytes = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_file(&out);
    bytes
}

fn le_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn le_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn le_i32(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn le_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// The single section's `(file offset, virtual address, raw size)`.
fn section(pe: &[u8]) -> (usize, u32, usize) {
    let sect = le_u32(pe, 0x3c) as usize + 24 + 240; // PE sig + COFF(24) + optional header(240)
    let vaddr = le_u32(pe, sect + 12);
    let raw_size = le_u32(pe, sect + 16) as usize;
    let praw = le_u32(pe, sect + 20) as usize;
    (praw, vaddr, raw_size)
}

/// Map an RVA to a file offset through the single section.
fn rva_to_off(pe: &[u8], rva: u32) -> usize {
    let (praw, vaddr, _) = section(pe);
    (rva - vaddr) as usize + praw
}

/// File offset of the first occurrence of `needle` in the code section.
fn find_code(pe: &[u8], needle: &[u8]) -> usize {
    let (praw, _, raw) = section(pe);
    let code = &pe[praw..praw + raw];
    praw + code
        .windows(needle.len())
        .position(|w| w == needle)
        .unwrap_or_else(|| panic!("byte pattern {needle:02X?} not found in code"))
}

/// The `kernel32` function name a `call qword [rip+disp32]` at file offset `pos`
/// resolves to: follow the RIP-relative displacement to an IAT slot, then the
/// slot's RVA to its hint/name entry.
fn call_target(pe: &[u8], pos: usize) -> String {
    assert_eq!(
        &pe[pos..pos + 2],
        &[0xFF, 0x15],
        "expected `call [rip]` (FF 15) at {pos:#x}"
    );
    let (praw, vaddr, _) = section(pe);
    let next_rva = vaddr as i64 + (pos - praw) as i64 + 6; // RIP after the 6-byte call
    let slot_rva = (next_rva + le_i32(pe, pos + 2) as i64) as u32; // IAT slot
    let name_rva = le_u64(pe, rva_to_off(pe, slot_rva)) as u32; // -> hint/name entry
    let hn = rva_to_off(pe, name_rva) + 2; // skip the 2-byte hint
    let len = pe[hn..].iter().position(|&b| b == 0).unwrap();
    String::from_utf8_lossy(&pe[hn..hn + len]).into_owned()
}

#[test]
fn produces_a_valid_pe32plus_executable() {
    let pe = build_pe("return 42;");

    // DOS header: "MZ" and e_lfanew pointing at the PE signature.
    assert_eq!(&pe[0..2], b"MZ", "DOS magic");
    let coff = le_u32(&pe, 0x3c) as usize;
    assert_eq!(coff, 0x40, "e_lfanew");
    assert_eq!(&pe[coff..coff + 4], b"PE\0\0", "PE signature");

    // COFF header.
    assert_eq!(le_u16(&pe, coff + 4), 0x8664, "machine should be AMD64");
    assert_eq!(le_u16(&pe, coff + 6), 1, "one section");
    assert_eq!(le_u16(&pe, coff + 20), 240, "SizeOfOptionalHeader");

    // Optional header (PE32+).
    let opt = coff + 24;
    assert_eq!(le_u16(&pe, opt), 0x020B, "PE32+ magic");
    assert_eq!(
        le_u32(&pe, opt + 16),
        0x1000,
        "entry point at the section start"
    );
    assert_eq!(le_u64(&pe, opt + 24), 0x1_4000_0000, "ImageBase");
    assert_eq!(le_u16(&pe, opt + 68), 3, "Subsystem = console");

    // The one section is R+W+X and maps the image.
    let sect = opt + 240;
    assert_eq!(&pe[sect..sect + 5], b".text", "section name");
    assert_eq!(le_u32(&pe, sect + 12), 0x1000, "section VirtualAddress");
    assert_eq!(
        le_u32(&pe, sect + 36),
        0xE000_0020,
        "section is CODE|EXEC|READ|WRITE"
    );
}

#[test]
fn entry_is_framed_and_exits_via_exitprocess() {
    let pe = build_pe("return 42;");
    let (praw, _, _) = section(&pe);

    // The entry opens a frame (`push rbp; mov rbp, rsp`).
    assert_eq!(
        &pe[praw..praw + 4],
        &[0x55, 0x48, 0x89, 0xE5],
        "frame prologue"
    );

    // The exit shim: `mov ecx, eax; sub rsp, 32; call [rip]` -> ExitProcess.
    let shim = [0x89, 0xC1, 0x48, 0x83, 0xEC, 0x20, 0xFF, 0x15];
    let at = find_code(&pe, &shim);
    assert_eq!(call_target(&pe, at + 6), "ExitProcess");
}

#[test]
fn printing_lowers_to_getstdhandle_then_writefile() {
    let pe = build_pe("\"hi\\n\";");

    // The full `emit_write_stdout` shim, byte for byte (the two disp32s aside):
    //   sub rsp,72; mov [rsp+48],rsi; mov [rsp+56],rdx; mov ecx,-11; call [GetStdHandle]
    let head = [
        0x48, 0x83, 0xEC, 0x48, // sub rsp, 72
        0x48, 0x89, 0x74, 0x24, 0x30, // mov [rsp+48], rsi
        0x48, 0x89, 0x54, 0x24, 0x38, // mov [rsp+56], rdx
        0xB9, 0xF5, 0xFF, 0xFF, 0xFF, // mov ecx, -11
        0xFF, 0x15, // call [rip] (GetStdHandle)
    ];
    let start = find_code(&pe, &head);
    let gsh = start + head.len() - 2; // the FF 15 of the first call
    assert_eq!(call_target(&pe, gsh), "GetStdHandle");

    // Then marshal the WriteFile args and call it.
    let mid = [
        0x48, 0x89, 0xC1, // mov rcx, rax (handle)
        0x48, 0x8B, 0x54, 0x24, 0x30, // mov rdx, [rsp+48] (buf)
        0x4C, 0x8B, 0x44, 0x24, 0x38, // mov r8, [rsp+56] (len)
        0x4C, 0x8D, 0x4C, 0x24, 0x28, // lea r9, [rsp+40] (&written)
        0x48, 0xC7, 0x44, 0x24, 0x20, 0x00, 0x00, 0x00, 0x00, // mov qword [rsp+32], 0
        0xFF, 0x15, // call [rip] (WriteFile)
    ];
    let mid_at = gsh + 6; // just past the GetStdHandle call
    assert_eq!(
        &pe[mid_at..mid_at + mid.len()],
        &mid,
        "WriteFile marshaling"
    );
    let wf = mid_at + mid.len() - 2;
    assert_eq!(call_target(&pe, wf), "WriteFile");
    // ... and tear the frame down.
    assert_eq!(
        &pe[wf + 6..wf + 6 + 4],
        &[0x48, 0x83, 0xC4, 0x48],
        "add rsp, 72"
    );
}

#[test]
fn args_capture_calls_getcommandline() {
    // A program using ArgC/ArgV captures the command line at the entry: the first
    // `call [rip]` after the frame prologue is `GetCommandLineA`.
    let pe = build_pe("\"%d\\n\", ArgC();");
    // The capture opens with `sub rsp, 32; call [rip]` (shadow space + the call).
    let at = find_code(&pe, &[0x48, 0x83, 0xEC, 0x20, 0xFF, 0x15]);
    assert_eq!(call_target(&pe, at + 4), "GetCommandLineA");
}

#[test]
fn malloc_lowers_to_virtualalloc() {
    let pe = build_pe("U8 *p = MAlloc(16); p[0] = 7;");

    // The `emit_page_alloc` shim, byte for byte (disp32 aside):
    //   xor ecx,ecx; mov rdx,rsi; mov r8d,0x3000; mov r9d,4; sub rsp,32; call [VirtualAlloc]; add rsp,32
    let shim = [
        0x31, 0xC9, // xor ecx, ecx
        0x48, 0x89, 0xF2, // mov rdx, rsi
        0x41, 0xB8, 0x00, 0x30, 0x00, 0x00, // mov r8d, 0x3000
        0x41, 0xB9, 0x04, 0x00, 0x00, 0x00, // mov r9d, 4
        0x48, 0x83, 0xEC, 0x20, // sub rsp, 32
        0xFF, 0x15, // call [rip] (VirtualAlloc)
    ];
    let at = find_code(&pe, &shim);
    let call = at + shim.len() - 2;
    assert_eq!(call_target(&pe, call), "VirtualAlloc");
    assert_eq!(
        &pe[call + 6..call + 6 + 4],
        &[0x48, 0x83, 0xC4, 0x20],
        "add rsp, 32"
    );
}
