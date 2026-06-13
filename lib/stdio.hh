#ifndef _STDIO_HH
#define _STDIO_HH
// stdio.hh — C `<stdio.h>`: formatted output (the printf family + its private rendering
// core and correctly-rounded float formatter), formatted input (`SScan`, sscanf),
// character/line I/O (`PutChar`/`Puts`/`FGetC`/`GetChar`/`FGetS`/`GetLine`/`ReadLine`),
// the file-removal primitives (`Remove`/`Rename`), and path-based file helpers
// (`AppendFile`/`FileSize`).
//
// The printf family is ordinary HolyC: `VFmt` (below) walks the format string and renders
// each conversion into a sink (a fd via `StdWrite`, or a buffer); floats go through the
// base-2³² bignum formatter `FmtFloat`. There is no separate Rust formatter — the
// interpreter runs these same bodies and the backends compile them, so every target's
// output is byte-identical by construction. Include with `#include <stdio.hc>`.
//
// Note: a bare string statement (`"hi\n";`) lowers to a raw `StdWrite`, but the
// `"fmt", a, b` comma form desugars to a `Print(...)` call — so a program that prints
// must `#include <stdio.hc>`, exactly as in C (there is no implicit prelude).
//
//   Print(fmt, ...)               — printf to stdout.
//   StrPrint(dst, fmt, ...)       — sprintf into `dst`; returns `dst`.
//   StrNPrint(dst, cap, fmt, ...) — snprintf: bounded sprintf; returns the would-be length.
//   CatPrint(dst, fmt, ...)       — sprintf appended at `dst + StrLen(dst)`; returns `dst`.
//   MStrPrint(fmt, ...)           — asprintf into a fresh, growing heap buffer.
//
// File errors are returned Unix-style: a non-negative result on success, a negative
// `-errno` on failure.


// The single entry the native backends' print lowering calls. Formats `v` into `out`
// (NUL-terminated) and returns the length. `conv` is the conversion char
// (`'f'`/`'e'`/`'E'`/`'g'`/`'G'`). `flags` is the packed flag bits above. `width` and
// `prec` are the field width and precision. The output is byte-for-byte the
// interpreter's float rendering. The sign comes from the IEEE sign bit (so `-0.0` keeps
// its `-`) or the `+`/space flag, and zero-padding goes *after* the sign. User code
// prints floats via `Print`/`"%f", …`; this is the formatter's entry point and is
// `public` only so the float-formatter conformance tests can pin it byte-for-byte.
public I64 FmtFloat(U8 *out, F64 v, I64 conv, I64 flags, I64 width, I64 prec);

// =============================================================================
// printf family (public)
// =============================================================================

public U0 Print(U8 *fmt, ...);

// printf to an arbitrary file descriptor (`fprintf`, with the fd in place of the
// `FILE *`). Same conversions as `Print`; returns the number of bytes written.
public I64 FPrint(I64 fd, U8 *fmt, ...);

public U8 *StrPrint(U8 *dst, U8 *fmt, ...);

// Bounded sprintf (snprintf): format into `dst`, writing at most `cap` bytes including
// the terminating NUL, so `dst` is never overflowed. Returns the number of bytes that
// *would* have been written had `cap` been large enough (excluding the NUL), so a return
// value `>= cap` means the output was truncated. `cap <= 0` writes nothing at all (not
// even the NUL) but still returns the would-be length, for sizing a buffer first.
public I64 StrNPrint(U8 *dst, I64 cap, U8 *fmt, ...);

public U8 *CatPrint(U8 *dst, U8 *fmt, ...);

public U8 *MStrPrint(U8 *fmt, ...);

// =============================================================================
// character & line output (public)
// =============================================================================

// Write one byte (`putchar`) / a line (`puts`, with a trailing newline) to stdout, via the
// portable `StdWrite` (so they work on every target). `putchar` returns the byte, `puts` a
// non-negative count, or -1 on error — like C.
public I64 PutChar(I64 c);

public I64 Puts(U8 *s);

// Write one byte / a string to an arbitrary fd (`fputc`/`fputs`, with the fd in place
// of the `FILE *`; note the C argument order, value first). `FPutC` returns the byte,
// `FPutS` a non-negative count, or -1 on error — like C. Neither adds a newline.
public I64 FPutC(I64 c, I64 fd);

public I64 FPutS(U8 *s, I64 fd);

// =============================================================================
// character & line input (public)
// =============================================================================
//
// Built on the `Read` primitive, a byte at a time — there is no buffered `FILE*`
// stream yet, so these read directly. `STDIN` (fd 0) is the program's standard input.

// Next byte from `fd` (0..255), or -1 at end of file. Like C's fgetc / getc.
public I64 FGetC(I64 fd);

// Next byte from stdin, or -1 at EOF. Like C's getchar.
public I64 GetChar();

// Read a line from `fd` into `buf` (capacity `cap`): up to `cap - 1` bytes, stopping
// after a newline (which is kept) or at EOF, then NUL-terminate. Returns `buf`, or
// NULL if EOF is reached before any byte is read. Like C's fgets.
public U8 *FGetS(U8 *buf, I64 cap, I64 fd);

// POSIX getline: read a whole line from `fd` (including the trailing newline) into
// *`line`, growing the buffer as needed; *`cap` tracks its allocated size. Pass
// `*line = NULL, *cap = 0` to have it allocated for you. Returns the number of bytes
// read (excluding the NUL), or -1 at EOF with nothing read. The caller owns *`line`
// (`Free` it). Grows with `MAlloc`/`MemCpy`/`Free`, so stdio needs no `<stdlib.hc>`.
public I64 GetLine(U8 **line, I64 *cap, I64 fd);

// Read one line from `fd` into a fresh heap buffer with the trailing newline stripped,
// or NULL at EOF. The caller owns the buffer (`Free` it). An ergonomic HolyC sibling
// of `GetLine`.
public U8 *ReadLine(I64 fd);

public I64 SScan(U8 *buf, U8 *fmt, ...);

// Streaming scanf over stdin: like `SScan`, but reading lines from `STDIN` as the
// format needs them, and carrying unconsumed input over to the next `Scan` call — so
// `Scan("%d", &a); Scan("%d", &b);` against the input "1 2\n" assigns both, and a
// conversion list may span lines ("%d %d" reads "1\n2\n"), like C's scanf. Returns the
// assigned-field count, or -1 if end of input arrives before anything is assigned.
// Internally it rescans the accumulated input from the start whenever the format runs
// out of input mid-way, which is idempotent (the prefix parses identically), then
// keeps only the tail past what the format consumed.
public I64 Scan(U8 *fmt, ...);

// =============================================================================
// file removal (intrinsics) + path-based file helpers
// =============================================================================
//
// `Remove`/`Rename` are impure OS calls (`unlink`/`rename`), lowered to syscalls
// freestanding and to libc on Darwin. Each returns 0 on success, or a negative `-errno`.

public I64 Remove(U8 *path);                 // delete a file
public I64 Rename(U8 *oldpath, U8 *newpath); // rename/move

// Size of `path` in bytes, or -errno.
public I64 FileSize(U8 *path);

// Append `n` bytes to `path` (creating it, mode 0644). Returns 0, or -errno.
public I64 AppendFile(U8 *path, U8 *buf, I64 n);

#endif
