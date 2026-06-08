//! A tree-walking interpreter for HolyC.
//!
//! It executes a parsed program directly. Run semantic analysis first: the
//! interpreter assumes a well-formed program. It only reports faults it hits at
//! run time, such as division by zero, a null dereference, or a missing function
//! body.
//!
//! Value and memory model: most storage is a *cell* (`Rc<RefCell<Value>>`).
//! Variables, class fields, and array elements are all cells. So `&x` is a handle
//! to a cell, `*p` reads and writes through it, and `p->field` / `a[i]` resolve to
//! cells (via [`Place::Cell`]).
//!
//! `MAlloc` of an integer or float element type is the exception: it returns a raw
//! byte buffer ([`Region::Heap`]). Typed accesses serialize `sizeof(T)` bytes
//! through it ([`Place::Bytes`]), so the heap is genuinely byte-addressable and
//! type punning behaves like the native heap. On a byte-heap pointer, arithmetic
//! and indexing scale by the element size; on a cell pointer they step element by
//! element.
//!
//! A `union` instance is likewise a shared byte buffer ([`Value::Union`]). Its
//! fields overlap and alias, so writing one field and reading another sees the
//! same bytes. Pointer and class union fields are the exception: they can't be
//! serialized, and are unsupported.
//!
//! HolyC's implicit print is honoured. A bare string-literal expression statement
//! prints itself, and `"fmt", args…` formats and prints (a `Comma` whose first
//! element is a string).

use std::path::{Path, PathBuf};

// ---- cross-platform OS shims ----
//
// The interpreter emulates HolyC's POSIX-flavoured fd/file/process primitives over
// `std`. HolyC paths arrive as raw NUL-terminated byte strings, and file ops carry
// Unix mode bits. The few spots that need Unix-only `std` APIs are funneled through
// these shims, so the tools also build and run on non-Unix hosts (Windows). There,
// the mode bits are ignored and there is no POSIX uid/gid.

/// Build a filesystem path from HolyC's raw path bytes. On Unix the bytes pass
/// straight through (`OsStr::from_bytes`). Elsewhere they are read as UTF-8, which
/// is fine because HolyC paths are ASCII in practice.
#[cfg(unix)]
pub(crate) fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    Path::new(std::ffi::OsStr::from_bytes(bytes)).to_path_buf()
}
#[cfg(not(unix))]
pub(crate) fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// The raw bytes of a path (the inverse of [`path_from_bytes`]).
#[cfg(unix)]
pub(crate) fn path_to_bytes(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().to_vec()
}
#[cfg(not(unix))]
pub(crate) fn path_to_bytes(p: &Path) -> Vec<u8> {
    p.to_string_lossy().into_owned().into_bytes()
}

/// Apply a Unix permission `mode` to a file being created. A no-op on platforms
/// without Unix mode bits (Windows).
#[cfg(unix)]
pub(crate) fn set_open_mode(opts: &mut std::fs::OpenOptions, mode: u32) {
    use std::os::unix::fs::OpenOptionsExt;
    opts.mode(mode);
}
#[cfg(not(unix))]
pub(crate) fn set_open_mode(_opts: &mut std::fs::OpenOptions, _mode: u32) {}

/// `mkdir(path, mode)`. The `mode` is applied on Unix and ignored elsewhere.
#[cfg(unix)]
pub(crate) fn mkdir_with_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(mode).create(path)
}
#[cfg(not(unix))]
pub(crate) fn mkdir_with_mode(path: &Path, _mode: u32) -> std::io::Result<()> {
    std::fs::DirBuilder::new().create(path)
}

/// Normalise a host OS `errno` to the Linux-canonical numbering the `<errno.hc>`
/// constants use. On a Linux host `raw_os_error()` is already canonical (identity);
/// on macOS the Darwin codes that differ are remapped, so the interpreter (the oracle)
/// agrees with what the Darwin native binary returns (which remaps the same way). See
/// [`crate::intrinsics::DARWIN_TO_LINUX_ERRNO`].
#[cfg(target_os = "macos")]
pub(crate) fn norm_errno(raw: i64) -> i64 {
    crate::intrinsics::darwin_to_linux_errno(raw)
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn norm_errno(raw: i64) -> i64 {
    raw
}

/// The parent process id, or 0 where `std` can't report it portably, i.e. Windows.
#[cfg(unix)]
pub(crate) fn parent_pid() -> u32 {
    std::os::unix::process::parent_id()
}
#[cfg(not(unix))]
pub(crate) fn parent_pid() -> u32 {
    0
}

/// The real user/group id, read via libc on Unix. Returns 0 where there is no
/// POSIX uid/gid.
#[cfg(unix)]
pub(crate) fn get_uid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}
#[cfg(unix)]
pub(crate) fn get_gid() -> u32 {
    unsafe extern "C" {
        fn getgid() -> u32;
    }
    unsafe { getgid() }
}
#[cfg(not(unix))]
pub(crate) fn get_uid() -> u32 {
    0
}
#[cfg(not(unix))]
pub(crate) fn get_gid() -> u32 {
    0
}

/// Process CPU time in nanoseconds (`CpuNS`), via libc `clock_gettime` over the
/// process-CPU-time clock. There is no `std` accessor for it. The clock id differs by
/// host (Linux 2, macOS 12). Returns 0 where there is no POSIX CPU clock.
#[cfg(unix)]
pub(crate) fn cpu_ns() -> i64 {
    #[cfg(target_os = "macos")]
    const CPU_CLOCK: i32 = 12; // CLOCK_PROCESS_CPUTIME_ID on macOS
    #[cfg(not(target_os = "macos"))]
    const CPU_CLOCK: i32 = 2; // CLOCK_PROCESS_CPUTIME_ID on Linux
    #[repr(C)]
    struct Ts {
        sec: i64,
        nsec: i64,
    }
    unsafe extern "C" {
        fn clock_gettime(id: i32, ts: *mut Ts) -> i32;
    }
    let mut ts = Ts { sec: 0, nsec: 0 };
    unsafe { clock_gettime(CPU_CLOCK, &mut ts) };
    ts.sec * 1_000_000_000 + ts.nsec
}
#[cfg(not(unix))]
pub(crate) fn cpu_ns() -> i64 {
    0
}

use crate::ast::*;
use crate::codegen::CodegenError;

/// A mutable storage location.

/// An interpreter file descriptor: a reserved-but-unconnected socket, a live TCP
/// stream, or an open file. `Read`/`Write` go to the stream or file; `LSeek` works
/// only on a file.
pub(crate) enum FdObj {
    PendingSocket,
    Tcp(std::net::TcpStream),
    File(std::fs::File),
}

pub fn run_to_string(program: &Program) -> Result<String, CodegenError> {
    run_to_string_with_input(program, &[])
}

/// Run a program and capture its output via the **SSA IR interpreter** (the conformance
/// oracle): lower to IR ([`crate::lower`]) and execute it ([`crate::irinterp`]). `input`
/// is the program's standard input (fd 0).
pub fn run_to_string_with_input(program: &Program, input: &[u8]) -> Result<String, CodegenError> {
    if let Some(e) = crate::sema::check_program(program).into_iter().next() {
        return Err(CodegenError::at(
            e.pos,
            format!("semantic error: {}", e.message),
        ));
    }
    let (layouts, layout_errs) = crate::layout::compute(program);
    if let Some(e) = layout_errs.into_iter().next() {
        return Err(CodegenError::at(
            e.pos,
            format!("layout error: {}", e.message),
        ));
    }
    let ir = crate::lower::lower(program, &layouts)?;
    let mut interp = crate::irinterp::IrInterp::new(&ir);
    interp.set_input(Box::new(std::io::Cursor::new(input.to_vec())));
    interp.run_program()
}
