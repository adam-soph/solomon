//! The `x86_64-pc-windows` target: `kernel32` imports and a self-contained PE
//! executable.
//!
//! The x86-64 code generation is shared with the Linux target through the parent
//! module's [`OsTarget`] seam; this module supplies only the Windows policy.
//!
//! The three OS seams lower to `kernel32` calls through the import address table:
//! `ExitProcess`, `VirtualAlloc`, and `GetStdHandle` plus `WriteFile`. Calls are
//! marshaled into the Microsoft x64 ABI — args in `rcx`/`rdx`/`r8`/`r9`, a
//! 32-byte shadow area, and 16-byte stack alignment at the call.
//!
//! The container is a hand-built PE with a `kernel32.dll` import directory, and
//! no linker, like the Linux ELF. Code, strings, the import table, and the BSS
//! share one R+W+X section that maps 1:1 with the file, so the encoder's
//! RIP-relative `target - (pos+4)` references resolve unchanged.

use std::path::PathBuf;

use super::{Asm, FileOp, OsTarget, R8, R9, R10, R11, R15, RAX, RCX, RDI, RDX, RSI};
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

    /// Calls a `kernel32` import with rsp 16-aligned and a 32-byte shadow area,
    /// as the MS x64 ABI requires. The call site's alignment is unknown, so the
    /// caller's rsp is saved in r15 first; r15 is non-volatile and survives the
    /// call. Arguments must already be in `rcx`/`rdx`/`r8`/`r9`.
    fn call_aligned(&mut self, asm: &mut Asm, name: &'static str) {
        let i = self.extern_idx(name);
        asm.mov_rr(R15, super::RSP); // save rsp
        asm.and_ri(super::RSP, -16); // 16-align
        asm.emit(&[0x48, 0x83, 0xEC, 0x20]); // sub rsp, 32 (shadow)
        asm.call_extern(i);
        asm.mov_rr(super::RSP, R15); // restore rsp
    }

    /// Lowers `Read`/`Write` to `ReadFile`/`WriteFile(hFile=rdi, buf=rsi, n=rdx,
    /// &done, NULL)`. Returns the byte count, or -1 on failure. Uses the same
    /// 72-byte frame as `emit_std_write`, the established WriteFile shim, taking
    /// the handle from `rdi`.
    fn emit_read_write(&mut self, asm: &mut Asm, func: &'static str) {
        // Force rsp to 16-byte alignment: save it in the non-volatile r15,
        // `and rsp,-16`, then reserve a 16-multiple frame. The call then lands on
        // a 16-aligned rsp regardless of the body's alignment. Some kernel32
        // functions fault on a misaligned stack.
        let wf = self.extern_idx(func);
        asm.mov_rr(R15, super::RSP); // save rsp (non-volatile; survives the call)
        asm.and_ri(super::RSP, -16); // 16-align
        asm.emit(&[0x48, 0x83, 0xEC, 0x40]); // sub rsp, 64 (32 shadow + arg/scratch slots)
        asm.emit(&[0x48, 0x89, 0x74, 0x24, 0x30]); // mov [rsp+48], rsi  (save buf)
        asm.emit(&[0x48, 0x89, 0x54, 0x24, 0x38]); // mov [rsp+56], rdx  (save n)
        asm.mov_rr(RCX, RDI); // hFile = handle
        asm.emit(&[0x48, 0x8B, 0x54, 0x24, 0x30]); // mov rdx, [rsp+48]  (lpBuffer)
        asm.emit(&[0x4C, 0x8B, 0x44, 0x24, 0x38]); // mov r8, [rsp+56]   (nBytes)
        asm.emit(&[0x4C, 0x8D, 0x4C, 0x24, 0x28]); // lea r9, [rsp+40]   (&done)
        asm.emit(&[0x48, 0xC7, 0x44, 0x24, 0x20, 0x00, 0x00, 0x00, 0x00]); // [rsp+32]=NULL
        asm.call_extern(wf);
        let fail = asm.new_label();
        let done = asm.new_label();
        asm.test_rr(RAX, RAX); // BOOL
        asm.je(fail);
        asm.emit(&[0x8B, 0x44, 0x24, 0x28]); // mov eax, [rsp+40]  (bytes done → rax)
        asm.jmp(done);
        asm.place(fail);
        asm.mov_ri(RAX, -1);
        asm.place(done);
        asm.mov_rr(super::RSP, R15); // restore rsp
    }

    /// Lowers `Open(path, flags, mode)` to `CreateFileA`. Translates the `fcntl.hc`
    /// open flags (`O_RDONLY`/`O_WRONLY`/`O_RDWR` plus `O_CREAT`/`O_TRUNC`) into
    /// the Win32 access mask (r10) and creation disposition (r11). `mode` is
    /// ignored, as Windows has no POSIX permission bits. Returns the HANDLE, or
    /// `INVALID_HANDLE_VALUE` (-1) on failure.
    fn emit_open(&mut self, asm: &mut Asm) {
        let cf = self.extern_idx("CreateFileA");
        // access (r10d): GENERIC_READ (0x80000000) unless O_WRONLY; GENERIC_WRITE
        // (0x40000000) unless O_RDONLY. `flags & 3` is the access-mode field.
        asm.mov_ri(R10, 0);
        let skip_r = asm.new_label();
        asm.mov_rr(RAX, RSI);
        asm.and_ri(RAX, 3);
        asm.cmp_ri(RAX, 1); // O_WRONLY → no read
        asm.je(skip_r);
        asm.alu_ri(1, R10, 0x8000_0000u32 as i32); // or r10d, GENERIC_READ
        asm.place(skip_r);
        let skip_w = asm.new_label();
        asm.mov_rr(RAX, RSI);
        asm.and_ri(RAX, 3);
        asm.cmp_ri(RAX, 0); // O_RDONLY → no write
        asm.je(skip_w);
        asm.alu_ri(1, R10, 0x4000_0000); // or r10d, GENERIC_WRITE
        asm.place(skip_w);
        // disposition (r11d): O_CREAT (0x40) / O_TRUNC (0x200) →
        //   creat&trunc=CREATE_ALWAYS(2), creat=OPEN_ALWAYS(4),
        //   trunc=TRUNCATE_EXISTING(5), else OPEN_EXISTING(3).
        asm.mov_ri(R11, 3);
        let nocreat = asm.new_label();
        let dispdone = asm.new_label();
        asm.mov_rr(RAX, RSI);
        asm.and_ri(RAX, 0x40);
        asm.cmp_ri(RAX, 0);
        asm.je(nocreat);
        let creat_notrunc = asm.new_label();
        asm.mov_rr(RAX, RSI);
        asm.and_ri(RAX, 0x200);
        asm.cmp_ri(RAX, 0);
        asm.je(creat_notrunc);
        asm.mov_ri(R11, 2); // CREATE_ALWAYS
        asm.jmp(dispdone);
        asm.place(creat_notrunc);
        asm.mov_ri(R11, 4); // OPEN_ALWAYS
        asm.jmp(dispdone);
        asm.place(nocreat);
        asm.mov_rr(RAX, RSI);
        asm.and_ri(RAX, 0x200);
        asm.cmp_ri(RAX, 0);
        asm.je(dispdone); // stays OPEN_EXISTING
        asm.mov_ri(R11, 5); // TRUNCATE_EXISTING
        asm.place(dispdone);
        // CreateFileA(path, access, share=3, sec=NULL, disposition, NORMAL=0x80, tmpl=NULL).
        // Force rsp to 16-byte alignment. CreateFileA faults on a misaligned
        // stack, unlike the simpler WriteFile. r10/r11/rdi are registers, so the
        // rsp moves don't disturb them.
        asm.mov_rr(R15, super::RSP); // save rsp (non-volatile; survives the call)
        asm.and_ri(super::RSP, -16); // 16-align
        asm.emit(&[0x48, 0x83, 0xEC, 0x40]); // sub rsp, 64 (32 shadow + 3 stack args)
        asm.mov_rr(RCX, RDI); // lpFileName = path
        asm.mov_rr(RDX, R10); // dwDesiredAccess
        asm.mov_ri(R8, 3); // dwShareMode = FILE_SHARE_READ|WRITE
        asm.mov_ri(R9, 0); // lpSecurityAttributes = NULL
        asm.emit(&[0x4C, 0x89, 0x5C, 0x24, 0x20]); // mov [rsp+32], r11 (dwCreationDisposition)
        asm.store_rsp_imm(40, 0x80); // [rsp+40] = FILE_ATTRIBUTE_NORMAL
        asm.store_rsp_imm(48, 0); // [rsp+48] = hTemplateFile = NULL
        asm.call_extern(cf);
        asm.mov_rr(super::RSP, R15); // restore rsp
    }
}

impl OsTarget for WindowsTarget {
    fn emit_exit(&mut self, asm: &mut Asm) {
        // ExitProcess(uExitCode = eax). rsp is already 16-aligned here, inside the
        // entry frame, so a 32-byte shadow area keeps it aligned at the call. The
        // call does not return.
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

    fn emit_std_write(&mut self, asm: &mut Asm) {
        // StdWrite(fd, buf, n): rdi=fd, rsi=buf, rdx=n. Pick the handle from fd
        // (fd 2 ⇒ STD_ERROR_HANDLE −12, else STD_OUTPUT_HANDLE −11), then call
        // WriteFile(handle, buf, n, &written, NULL) and return the written count
        // in rax. Uses the same 72-byte frame as emit_write_stdout, so rsp stays
        // 16-aligned at each call.
        let gsh = self.extern_idx("GetStdHandle");
        let wf = self.extern_idx("WriteFile");
        asm.emit(&[0x48, 0x83, 0xEC, 0x48]); // sub rsp, 72
        asm.emit(&[0x48, 0x89, 0x74, 0x24, 0x30]); // mov [rsp+48], rsi  (save buf)
        asm.emit(&[0x48, 0x89, 0x54, 0x24, 0x38]); // mov [rsp+56], rdx  (save len)
        // ecx = (rdi == 2) ? STD_ERROR_HANDLE : STD_OUTPUT_HANDLE
        asm.emit(&[0xB9, 0xF5, 0xFF, 0xFF, 0xFF]); // mov ecx, -11 (STD_OUTPUT_HANDLE)
        asm.emit(&[0x48, 0x83, 0xFF, 0x02]); // cmp rdi, 2
        asm.emit(&[0x75, 0x05]); // jne +5 (keep stdout handle)
        asm.emit(&[0xB9, 0xF4, 0xFF, 0xFF, 0xFF]); // mov ecx, -12 (STD_ERROR_HANDLE)
        asm.call_extern(gsh); // call [GetStdHandle] -> rax = handle
        asm.emit(&[0x48, 0x89, 0xC1]); // mov rcx, rax          (hFile)
        asm.emit(&[0x48, 0x8B, 0x54, 0x24, 0x30]); // mov rdx, [rsp+48]  (lpBuffer)
        asm.emit(&[0x4C, 0x8B, 0x44, 0x24, 0x38]); // mov r8, [rsp+56]   (nBytes)
        asm.emit(&[0x4C, 0x8D, 0x4C, 0x24, 0x28]); // lea r9, [rsp+40]   (&written)
        asm.emit(&[0x48, 0xC7, 0x44, 0x24, 0x20, 0x00, 0x00, 0x00, 0x00]); // mov qword [rsp+32], 0
        asm.call_extern(wf); // call [WriteFile]
        asm.emit(&[0x8B, 0x44, 0x24, 0x28]); // mov eax, [rsp+40]  (written → rax)
        asm.emit(&[0x48, 0x83, 0xC4, 0x48]); // add rsp, 72
    }

    fn is_posix(&self) -> bool {
        false // the Windows PE has no Linux syscalls
    }

    fn emit_fileop(&mut self, asm: &mut Asm, op: FileOp) {
        // The fd args arrive in the System V registers (rdi/rsi/rdx). Each op maps
        // to a `kernel32` call under the MS x64 ABI: args in rcx/rdx/r8/r9, stack
        // args at [rsp+32] and up, a 32-byte shadow area, and rsp 16-aligned at
        // the call. The "fd" is a Win32 HANDLE. Results follow the `unistd.hc`
        // contract: a count, offset, or HANDLE; 0; or a negative error.
        match op {
            FileOp::Open => self.emit_open(asm), // CreateFileA, with fcntl.hc flag → Win32 translation
            FileOp::Read => self.emit_read_write(asm, "ReadFile"),
            FileOp::Write => self.emit_read_write(asm, "WriteFile"),
            FileOp::Close => {
                // CloseHandle(hObject = handle). BOOL → 0 (success) / -1 (failure).
                asm.mov_rr(RCX, RDI);
                self.call_aligned(asm, "CloseHandle");
                let fail = asm.new_label();
                let done = asm.new_label();
                asm.test_rr(RAX, RAX);
                asm.je(fail);
                asm.xor_rr(RAX, RAX); // success → 0
                asm.jmp(done);
                asm.place(fail);
                asm.mov_ri(RAX, -1);
                asm.place(done);
            }
            FileOp::LSeek => {
                // SetFilePointerEx(hFile, liDistanceToMove=off, lpNewFilePointer=&newpos,
                // dwMoveMethod=whence). unistd.hc SEEK_SET/CUR/END (0/1/2) == FILE_BEGIN/
                // CURRENT/END. BOOL → newpos (64-bit) / -1. `newpos` is the [rsp+40] slot.
                let wf = self.extern_idx("SetFilePointerEx");
                asm.mov_rr(R15, super::RSP); // save rsp (non-volatile; survives the call)
                asm.and_ri(super::RSP, -16); // 16-align
                asm.emit(&[0x48, 0x83, 0xEC, 0x40]); // sub rsp, 64 (shadow + &newpos slot)
                asm.mov_rr(R9, RDX); // dwMoveMethod = whence (before rdx is reused)
                asm.mov_rr(RCX, RDI); // hFile = handle
                asm.mov_rr(RDX, RSI); // liDistanceToMove = off (by value)
                asm.emit(&[0x4C, 0x8D, 0x44, 0x24, 0x28]); // lea r8, [rsp+40] (&newpos)
                asm.call_extern(wf);
                let fail = asm.new_label();
                let done = asm.new_label();
                asm.test_rr(RAX, RAX);
                asm.je(fail);
                asm.emit(&[0x48, 0x8B, 0x44, 0x24, 0x28]); // mov rax, [rsp+40] (newpos)
                asm.jmp(done);
                asm.place(fail);
                asm.mov_ri(RAX, -1);
                asm.place(done);
                asm.mov_rr(super::RSP, R15); // restore rsp
            }
        }
    }

    fn emit_unix_ns(&mut self, asm: &mut Asm, ft: i32) {
        // GetSystemTimePreciseAsFileTime(&filetime): 100 ns ticks since 1601-01-01.
        // Convert to ns since the Unix epoch: (ticks - 1601→1970 offset) * 100.
        asm.lea_global(RCX, ft); // arg1 = &filetime
        self.call_aligned(asm, "GetSystemTimePreciseAsFileTime");
        asm.lea_global(RCX, ft);
        asm.load_qword_at(RAX, RCX); // rax = filetime ticks
        asm.mov_ri64(RCX, 116_444_736_000_000_000); // 1601→1970 in 100 ns units
        asm.sub_rr(RAX, RCX);
        asm.imul_rax_imm32(100); // ticks → ns
    }

    fn emit_mono_ns(&mut self, asm: &mut Asm, _scratch: i32) {
        // GetTickCount64() -> ms since boot (monotonic); *1e6 = ns.
        self.call_aligned(asm, "GetTickCount64");
        asm.imul_rax_imm32(1_000_000);
    }

    fn emit_sleep(&mut self, asm: &mut Asm, _scratch: i32) {
        // rax = ns; Sleep(DWORD ms = ns / 1e6).
        asm.mov_ri(RCX, 1_000_000);
        asm.div_rcx(); // rax = ms
        asm.mov_rr(RCX, RAX); // arg1 = ms
        self.call_aligned(asm, "Sleep");
    }

    fn emit_capture_env(&mut self, asm: &mut Asm, envp_off: i32) {
        // Windows has no `envp` array: its environment is a double-NUL-terminated block
        // of "KEY=VALUE\0" strings from GetEnvironmentStringsA. Build a NULL-terminated
        // pointer array (`U8 **EnvP`) over that block — each entry points straight into
        // the block (already NUL-separated, no copy). This runs at the entry before any
        // program code, so it may clobber any register. (`stdlib.hc`'s `Getenv` and
        // `vec.hc`'s `Environ` are pure HolyC over `EnvP`, so they now work on Windows too.)
        let ges = self.extern_idx("GetEnvironmentStringsA");
        let va = self.extern_idx("VirtualAlloc");

        // rsi = GetEnvironmentStringsA()  (rsi is non-volatile, so it survives VirtualAlloc)
        asm.emit(&[0x48, 0x83, 0xEC, 0x20]); // sub rsp, 32 (shadow)
        asm.call_extern(ges);
        asm.emit(&[0x48, 0x83, 0xC4, 0x20]); // add rsp, 32
        asm.mov_rr(RSI, RAX);

        // rdi = VirtualAlloc(NULL, 4096, MEM_COMMIT|MEM_RESERVE, PAGE_READWRITE)
        asm.xor_rr(RCX, RCX);
        asm.mov_ri(RDX, 0x1000);
        asm.mov_ri(R8, 0x3000);
        asm.mov_ri(R9, 4);
        asm.emit(&[0x48, 0x83, 0xEC, 0x20]); // sub rsp, 32
        asm.call_extern(va);
        asm.emit(&[0x48, 0x83, 0xC4, 0x20]); // add rsp, 32
        asm.mov_rr(RDI, RAX); // rdi = envp array base

        // EnvP slot = &array
        asm.lea_global(RCX, envp_off);
        asm.store_qword_at(RCX, RDI);

        // Collect a pointer to each "KEY=VALUE" string, up to the empty string (the
        // second NUL of the double terminator). Strings stay in place — no in-place
        // edits, since the block is already NUL-separated. The leading-`=` entries
        // Windows prepends (the per-drive cwd, e.g. `=C:=C:\…`) are skipped, so `EnvP`
        // matches the interpreter's `std::env` view (POSIX-style "KEY=VALUE").
        asm.xor_rr(R8, R8); // count = 0
        let loop_l = asm.new_label();
        let advance = asm.new_label(); // skip-store target + scan-loop top
        let done = asm.new_label();

        asm.place(loop_l);
        asm.load_byte_zx(RAX, RSI); // al = *cursor
        asm.test_rr(RAX, RAX);
        asm.je(done); // empty string => end of the block
        asm.cmp_ri(RAX, 0x3D); // '=' : a per-drive cwd entry — skip it
        asm.je(advance);
        asm.store_qword_idx8(RDI, R8, RSI); // envp[count] = cursor
        asm.inc_r(R8);
        asm.place(advance); // advance past this string and its NUL
        asm.load_byte_zx(RAX, RSI);
        asm.inc_r(RSI);
        asm.test_rr(RAX, RAX);
        asm.jne(advance);
        asm.jmp(loop_l);

        asm.place(done);
        asm.xor_rr(RAX, RAX);
        asm.store_qword_idx8(RDI, R8, RAX); // NULL-terminate the array
    }

    fn emit_capture_args(&mut self, asm: &mut Asm, argc_off: i32, argv_off: i32) {
        // Windows hands the entry no argv, so build one from GetCommandLineA. Get
        // the command line, allocate a page for the argv pointer array, then split
        // the line on spaces in place, NUL-terminating each token. This runs at the
        // entry before any program code, so it may clobber any register.
        let gcl = self.extern_idx("GetCommandLineA");
        let va = self.extern_idx("VirtualAlloc");

        // rsi = GetCommandLineA()  (rsi is non-volatile, so it survives VirtualAlloc)
        asm.emit(&[0x48, 0x83, 0xEC, 0x20]); // sub rsp, 32 (shadow)
        asm.call_extern(gcl);
        asm.emit(&[0x48, 0x83, 0xC4, 0x20]); // add rsp, 32
        asm.mov_rr(RSI, RAX);

        // rdi = VirtualAlloc(NULL, 4096, MEM_COMMIT|MEM_RESERVE, PAGE_READWRITE)
        asm.xor_rr(RCX, RCX);
        asm.mov_ri(RDX, 0x1000);
        asm.mov_ri(R8, 0x3000);
        asm.mov_ri(R9, 4);
        asm.emit(&[0x48, 0x83, 0xEC, 0x20]); // sub rsp, 32
        asm.call_extern(va);
        asm.emit(&[0x48, 0x83, 0xC4, 0x20]); // add rsp, 32
        asm.mov_rr(RDI, RAX); // rdi = argv array base

        // argv slot = &array
        asm.lea_global(RCX, argv_off);
        asm.store_qword_at(RCX, RDI);

        // Split rsi into rdi[..], counting tokens in r8 (whitespace-separated).
        asm.xor_rr(R8, R8); // count = 0
        let skip = asm.new_label();
        let tok = asm.new_label();
        let scan = asm.new_label();
        let end_tok = asm.new_label();
        let done = asm.new_label();

        asm.place(skip); // skip spaces between tokens
        asm.load_byte_zx(RAX, RSI); // al = *cursor
        asm.cmp_ri(RAX, 0x20);
        asm.jne(tok);
        asm.inc_r(RSI);
        asm.jmp(skip);

        asm.place(tok);
        asm.test_rr(RAX, RAX);
        asm.je(done); // end of command line
        asm.store_qword_idx8(RDI, R8, RSI); // argv[count] = cursor
        asm.inc_r(R8);

        asm.place(scan); // advance to the next space (or end)
        asm.load_byte_zx(RAX, RSI);
        asm.test_rr(RAX, RAX);
        asm.je(done);
        asm.cmp_ri(RAX, 0x20);
        asm.je(end_tok);
        asm.inc_r(RSI);
        asm.jmp(scan);

        asm.place(end_tok);
        asm.store_byte_imm_at(RSI, 0); // NUL-terminate the token
        asm.inc_r(RSI);
        asm.jmp(skip);

        asm.place(done);
        asm.lea_global(RCX, argc_off);
        asm.store_qword_at(RCX, R8); // argc slot = count
    }

    fn wrap(&mut self, asm: Asm, bss: u64) -> Result<Vec<u8>, CodegenError> {
        let n = self.externs.len();
        // The import region is appended after the code and strings. Compute where
        // it will land so its internal RVAs (and the IAT) come out correct.
        let import_base = SECTION_RVA as usize + asm.code_len() + asm.strings_total();

        // Layout within the import region: the import directory table (one
        // descriptor plus a null terminator), the import lookup table, the import
        // address table (what the call sites target), then the hint/name table and
        // the DLL name.
        let idt_size = 40usize; // 2 × IMAGE_IMPORT_DESCRIPTOR (kernel32 + null)
        let thunks = (n + 1) * 8; // n names + a null terminator, 8 bytes each (PE32+)
        let ilt_off = idt_size;
        let iat_off = ilt_off + thunks;
        let hn_off = iat_off + thunks;

        // Hint/name entries: a 2-byte hint, the name, and a NUL, padded to an even
        // length. Record each function's RVA for the lookup and address tables.
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

/// Wraps the finished image in a minimal PE32+ executable. The image is
/// `[code | strings | import]`, with `bss` zero bytes following it in memory.
/// The PE has one R+W+X section that maps the image 1:1, plus an import directory
/// pointing at `kernel32.dll`.
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
