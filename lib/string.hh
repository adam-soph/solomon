#ifndef _STRING_HH
#define _STRING_HH
// string.hh — C `<string.h>`: operations on NUL-terminated byte strings (the `str*`
// family) and on raw memory blocks (the `mem*` family).
//
// Everything here is pure HolyC over raw byte pointers (`U8 *`). Byte values are `U8`
// (unsigned), so the `<` and `>` comparisons are unsigned, matching libc's `strcmp`
// family. Include with `#include <string.hc>`.
//
// The number <-> string conversions (`StrToI64`/`I64ToStr`/`StrToF64`/`F64ToStr`) are
// C's `atoi`/`atof` family, so they live in `<stdlib.hc>`, not here. The generic
// allocators (`CAlloc`/`ReAlloc`) also live in `<stdlib.hc>`; the string duplicators
// `StrDup`/`StrNDup` (which `MAlloc` a copy) are `<string.h>` members and live here.

// `MAlloc` for the `StrDup`/`StrNDup` duplicators. The public home of the heap
// allocator is `<stdlib.hc>` (this prototype only lets `<string.hc>` stand alone
// without dragging all of `<stdlib.hc>` in; it is a redundant, allowed re-declaration).

// --- length & comparison (sign-normalised to -1/0/1) ---

public I64 StrLen(U8 *s);

// Length of `s`, but scanning at most `n` bytes (strnlen): stops at a NUL or after n.
public I64 StrNLen(U8 *s, I64 n);

public I64 StrCmp(U8 *a, U8 *b);

// Stock comparator for a `U8 *` (string-pointer) element. `Sort`/`VecSort`/
// `HmapSortKeys` over a `Vec<U8 *>` hand the comparator *pointers to elements*, i.e.
// `U8 **`, so it dereferences once before comparing the strings.
public I64 CmpStr(U8 **a, U8 **b);

public I64 StrNCmp(U8 *a, U8 *b, I64 n);

// Case-insensitive compares (ASCII), sign-normalised to -1/0/1, like POSIX
// strcasecmp / strncasecmp.
public I64 StrCaseCmp(U8 *a, U8 *b);

public I64 StrNCaseCmp(U8 *a, U8 *b, I64 n);

// --- copy & concatenate (return dst) ---

public U8 *StrCpy(U8 *dst, U8 *src);

// Copy up to n chars; NUL-pad to exactly n (no terminator past n), like strncpy.
public U8 *StrNCpy(U8 *dst, U8 *src, I64 n);

public U8 *StrCat(U8 *dst, U8 *src);

// Append up to n chars of src, then NUL-terminate (always), like strncat.
public U8 *StrNCat(U8 *dst, U8 *src, I64 n);

// --- search ---

// First occurrence of needle in haystack, or NULL. An empty needle matches at the
// start (strstr).
public U8 *StrFind(U8 *hay, U8 *needle);

// First / last `c` in str. The terminating NUL counts, so c == 0 finds it.
public U8 *StrChr(U8 *s, I64 c);

public U8 *StrLastChr(U8 *s, I64 c);

// Is byte c one of the NUL-terminated `set`'s characters?
public I64 StrInSet(U8 c, U8 *set);

// Length of the initial run of str whose chars are in / not in `set`.
public I64 StrSpn(U8 *s, U8 *set);

public I64 StrCSpn(U8 *s, U8 *set);

// First char of s that is in `set`, or NULL (strpbrk).
public U8 *StrPBrk(U8 *s, U8 *set);

// Tokenise `s` by any char in `delim`, reentrantly (strtok_r). The first call passes the
// string; later calls pass NULL to continue. `*save` carries the position between calls.
// Writes a NUL over each delimiter, so `s` is modified in place. Returns the next token,
// or NULL when there are none left.
public U8 *StrTokR(U8 *s, U8 *delim, U8 **save);
public U8 *StrTok(U8 *s, U8 *delim);

// strsep: split *`stringp` at the first char in `delim`. Returns the token before that
// delimiter and advances *`stringp` just past it (or to NULL when none remains). Unlike
// StrTok it does NOT merge adjacent delimiters, so it yields empty tokens — the right tool
// for fixed fields (CSV, `key=value`). Writes a NUL over the delimiter (modifies the
// string). Returns NULL only once *`stringp` is already NULL.
public U8 *StrSep(U8 **stringp, U8 *delim);

// --- in-place transforms (return str) ---

public U8 *StrToUpper(U8 *s);

public U8 *StrToLower(U8 *s);

public U8 *StrRev(U8 *s);

// --- allocate & duplicate (the caller owns the result; `Free` it) -------------

// strdup: a fresh heap copy of the NUL-terminated string `s` (including its NUL).
public U8 *StrDup(U8 *s);

// strndup: a fresh heap copy of at most n chars of `s`, always NUL-terminated.
public U8 *StrNDup(U8 *s, I64 n);

// --- raw memory (the `mem*` family) ------------------------------------------

public U8 *MemCpy(U8 *dst, U8 *src, I64 n);

// Copy bytes until `c` has been copied or n bytes are done. Returns the byte just past
// the copied `c` in dst, or NULL if `c` wasn't among the first n bytes (memccpy).
public U8 *MemCCpy(U8 *dst, U8 *src, I64 c, I64 n);

// Overlap-safe: copy backwards when dst is above src within the same region.
public U8 *MemMove(U8 *dst, U8 *src, I64 n);

public U8 *MemSet(U8 *dst, I64 c, I64 n);

// Sign-normalised to -1/0/1 (bytes compared unsigned), like the old builtin.
public I64 MemCmp(U8 *a, U8 *b, I64 n);

// First byte equal to `c` in buf[0..n], or NULL (memchr).
public U8 *MemFind(U8 *buf, I64 c, I64 n);

// First occurrence of needle[0..nlen] in hay[0..hlen], or NULL (memmem). An empty
// needle matches at the start.
public U8 *MemSearch(U8 *hay, I64 hlen, U8 *needle, I64 nlen);

#endif
