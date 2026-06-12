#ifndef _HEAP_HH
#define _HEAP_HH
// heap.hh — the base allocator primitives `MAlloc`/`Free` on their own, so the low-level
// modules that only need to allocate (`<string.hh>`'s `StrDup`, `<stdio.hh>`'s
// `MStrPrint`, `<threads.hh>`'s TLS) can get the declarations WITHOUT pulling all of
// `<stdlib.hh>` — whose generic `Sort`/`BSearch` would otherwise be dragged into every
// printing program and shadow a user's like-named functions. The public home is still
// `<stdlib.hh>`, which `#include`s this. `MAlloc`/`Free` are bodyless compiler primitives
// (an `mmap` bump allocator freestanding, libc `malloc`/`free` hosted), so there is no
// `heap.hc` implementation to pair.

public U8 *MAlloc(I64 n);
public U0 Free(U8 *ptr);

#endif
