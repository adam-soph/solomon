// windows.hc — Windows-only declarations (C analog: <windows.h>).
//
// This header is the home for genuinely platform-specific Win32 surface — the
// things that have no portable equivalent and so are kept out of the portable
// C-named headers. It is gated on the compiler-predefined `_WIN32` target macro
// (see `target_macros` in src/lib.rs), the same way `<windows.h>` is `#ifdef
// _WIN32` in C: `#include <windows.hc>` is harmless on every target, but the
// Windows-specific declarations only appear when compiling for the Windows target.
//
// The functions below are **primitive intrinsics**: bodyless prototypes the
// `x86_64-pc-windows` backend lowers to direct `kernel32` imports (a `Prim::WinCall`,
// see `intrinsics::win_import`), and the interpreter models over `std`. They are the
// raw Win32 API. The non-Windows backends reject them, but `#ifdef _WIN32` keeps them
// out of those compiles entirely.
//
// A `CreateFileA` HANDLE also interoperates with the portable fd primitives: on
// Windows `Read`/`Write`/`Close`/`LSeek` (`<unistd.hc>`) lower to the same `ReadFile`/
// `WriteFile`/`CloseHandle`/`SetFilePointerEx` kernel32 calls, so either API works on a
// handle.
#ifndef _WINDOWS_HC
#define _WINDOWS_HC

#ifdef _WIN32

// Win32 scalar aliases. A HANDLE is an opaque pointer-sized value; DWORD is 32-bit.
#define HANDLE I64
#define DWORD  U32

// Access rights (dwDesiredAccess), share modes, and creation dispositions for
// CreateFileA, plus the SetFilePointerEx move methods.
#define GENERIC_READ      0x80000000
#define GENERIC_WRITE     0x40000000
#define FILE_SHARE_READ   0x00000001
#define FILE_SHARE_WRITE  0x00000002
#define CREATE_NEW        1
#define CREATE_ALWAYS     2
#define OPEN_EXISTING     3
#define OPEN_ALWAYS       4
#define TRUNCATE_EXISTING 5
#define FILE_ATTRIBUTE_NORMAL 0x80
#define FILE_BEGIN   0
#define FILE_CURRENT 1
#define FILE_END     2
#define INVALID_HANDLE_VALUE -1

// CreateFileA returns the HANDLE (or INVALID_HANDLE_VALUE). ReadFile/WriteFile,
// SetFilePointerEx, and GetFileSizeEx return a BOOL (0 = failure) and report their
// result through an out-parameter pointer; CloseHandle returns a BOOL.
public HANDLE CreateFileA(U8 *name, I64 access, I64 share, U8 *sec,
                          I64 disposition, I64 flags, HANDLE template);
public I64 ReadFile(HANDLE h, U8 *buf, I64 n, DWORD *read, U8 *ovl);
public I64 WriteFile(HANDLE h, U8 *buf, I64 n, DWORD *written, U8 *ovl);
public I64 CloseHandle(HANDLE h);
public I64 SetFilePointerEx(HANDLE h, I64 distance, I64 *newpos, I64 method);
public I64 GetFileSizeEx(HANDLE h, I64 *size);

// Misc kernel32 queries.
public DWORD GetLastError();
public DWORD GetCurrentProcessId();

// --- the registry (advapi32) -------------------------------------------------
// The Windows registry — a hierarchical key/value store — has no POSIX equivalent at
// all, so it lives here and nowhere portable. An `HKEY` is an open-key handle; the
// predefined roots below are passed where one is expected. `REGSAM` is an access mask.
// Every Reg* call returns a LONG status (`ERROR_SUCCESS` = 0). These import from
// `advapi32.dll` (not kernel32), which the PE import table now supports per DLL.

#define HKEY  I64
#define REGSAM I64
// Predefined root keys (sign-extended 0x8000000x, as on 64-bit Windows).
#define HKEY_CLASSES_ROOT  -2147483648
#define HKEY_CURRENT_USER  -2147483647
#define HKEY_LOCAL_MACHINE -2147483646
// Access masks and value types.
#define KEY_READ      0x20019
#define KEY_WRITE     0x20006
#define KEY_ALL_ACCESS 0xF003F
#define REG_SZ    1
#define REG_DWORD 4
#define ERROR_SUCCESS 0

// Create or open `subkey` under `key`; the open handle is written to `*result` (and the
// created-vs-opened code to `*disposition`, which may be NULL). `cls`/`sec` are NULL.
public I64 RegCreateKeyExA(HKEY key, U8 *subkey, DWORD reserved, U8 *cls,
                           DWORD options, REGSAM sam, U8 *sec,
                           HKEY *result, DWORD *disposition);
// Set value `name` under the open `key` to `cbdata` bytes of `data` (type `REG_SZ`/…).
public I64 RegSetValueExA(HKEY key, U8 *name, DWORD reserved, DWORD ty,
                          U8 *data, DWORD cbdata);
// Read value `name`: the type → `*ty` (may be NULL), the bytes → `data`, and the byte
// count → `*cbdata` (in: buffer capacity; out: actual size). `reserved` is NULL.
public I64 RegQueryValueExA(HKEY key, U8 *name, DWORD *reserved, DWORD *ty,
                            U8 *data, DWORD *cbdata);
public I64 RegCloseKey(HKEY key);
public I64 RegDeleteKeyA(HKEY key, U8 *subkey);

#endif

#endif
