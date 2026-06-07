# solomon stdlib vs. the C standard library

How solomon's HolyC standard library (`lib/*.hc`) relates to ISO C's. solomon mirrors
C's **header layout and grouping**, but keeps **HolyC-PascalCase names** (`StrLen`, not
`strlen`) and a few deliberate semantic departures. Three buckets per area: **missing**
(in C, not in solomon), **different** (present but behaves unlike C), and **added** (in
solomon, not in C).

The quick name mapping: `malloc`→`MAlloc`, `free`→`Free`, `calloc`→`CAlloc`,
`realloc`→`ReAlloc`, `memcpy`→`MemCpy`, `strlen`→`StrLen`, `strstr`→`StrFind`,
`printf`→`Print`, `sprintf`→`StrPrint`, `qsort`→`Sort`, `bsearch`→`BSearch`,
`atoi`→`StrToI64`, `atof`→`StrToF64`, `exit`→`Exit`, `getenv`→`Getenv`, `isdigit`→`IsDigit`.

---

## Cross-cutting differences (apply everywhere)

- **No `errno`.** Failing calls return a negative `-errno` *as the value* (a non-negative
  result means success). There is no `errno` global, no `strerror`, no `perror`.
- **No `FILE *` / buffered streams.** I/O is file-descriptor based (`<unistd.hc>` /
  `<fcntl.hc>`) plus path-level convenience helpers; there is no `fopen`/`fread`/`fwrite`.
- **No formatted input.** There is no `scanf`/`sscanf`/`fscanf` family at all.
- **No wide chars, locales, or `float`.** Only the "C" locale, no `wchar_t`/`char16_t`,
  and the only floating type is `F64` (no `float`/`long double`, so no `sinf`/`sinl`).
- **Comparators normalize to `-1/0/1`.** `StrCmp`/`StrNCmp`/`MemCmp` return exactly
  `-1`, `0`, or `1` — not "the difference of the first differing bytes" that C permits.
- **`MAlloc`/`Free` are always in scope** (the `<builtin.hc>` prelude); the rest of
  `<stdlib.h>` needs `#include <stdlib.hc>`.

---

## Header-by-header

### `<string.h>` → `string.hc`
| | |
|---|---|
| **Have** | `StrLen`, `StrCmp`, `StrNCmp`, `StrCpy`, `StrNCpy`, `StrCat`, `StrFind` (`strstr`), `StrChr`, `StrLastChr` (`strrchr`), `StrSpn`, `StrCSpn`; `MemCpy`, `MemMove`, `MemSet`, `MemCmp`, `MemFind` (`memchr`), `MemSearch` (`memmem`) |
| **Missing** | `strncat`, `strpbrk`, `strtok` (no tokenizer), `strcoll`/`strxfrm` (locale), `strerror`, `strdup`/`strndup` |
| **Different** | `StrCmp`/`StrNCmp`/`MemCmp` are sign-normalized; byte values compare *unsigned* |
| **Added** | `StrInSet`, `StrToUpper`/`StrToLower`/`StrRev` (in-place transforms), `CmpStr` (a ready comparator), `MemSearch` (GNU `memmem`) |

### `<stdlib.h>` → `stdlib.hc` (+ `MAlloc`/`Free` in the prelude)
| | |
|---|---|
| **Have** | `MAlloc`, `Free`, `CAlloc` (`calloc`), `ReAlloc` (`realloc`); `StrToI64` (`atoi`/`atoll`), `StrToF64` (`atof`/`strtod`); `Sort` (`qsort`), `BSearch` (`bsearch`); `RandU64`/`SeedRand` (`rand`/`srand`); `Exit` (`exit`); `Getenv` (`getenv`) |
| **Missing** | `strtol`/`strtoul`/`strtoll` (no base or `endptr` parsing), `abort`, `atexit`, `system`, `setenv`/`putenv`/`unsetenv`, `aligned_alloc`, `div`/`ldiv`, `quick_exit`, the multibyte (`mbtowc` …) family, `RAND_MAX` |
| **Different** | `RandU64` returns a full 64-bit value (not `[0, RAND_MAX]`); `StrToI64` is base-10 only with no `endptr`/overflow signal; `Sort`/`BSearch` are **typed generics** (`Sort<T>`), not `void*` + element-size; `abs` lives in `<math.hc>` (and is generic, returning `I64`) |
| **Added** | `MSize` (a block's size), `HeapExtend` (in-place grow), `I64ToStr`, `F64ToStr` (shortest round-trip — C uses `sprintf("%g")`), stock comparators `CmpI64`/`CmpU64`/`CmpF64` |

### `<stdio.h>` → `stdio.hc`
| | |
|---|---|
| **Have** | `Print` (`printf`), `StrPrint` (`sprintf`), `MStrPrint` (`asprintf`), `CatPrint` (append-sprintf); `Remove` (`remove`), `Rename` (`rename`); path helpers `ReadFile`/`WriteFile`/`AppendFile`/`FileSize` |
| **Missing** | **The entire `FILE *` layer**: `fopen`/`fclose`/`fread`/`fwrite`/`fgets`/`fputs`/`fseek`/`ftell`/`fflush`/`feof`/`ferror`/`getchar`/`putchar`/`ungetc`/`setvbuf`. **All input**: `scanf`/`sscanf`/`fscanf`. Also `snprintf`'s size bound, `perror`, `tmpfile`/`tmpnam` |
| **Different** | `Print` returns `U0` (no printed-char count); `StrPrint` is **unbounded** (no size arg — caller must size the buffer, unlike `snprintf`); float formatting (`%f`/`%e`/`%g`) is solomon's own correctly-rounded formatter, byte-for-byte identical across the interpreter and every backend |
| **Added** | `MStrPrint` (grows a fresh heap buffer), `CatPrint`, the path-based file helpers, and the portable `StdWrite` (lives in `<unistd.hc>`; works on Windows too) |

Format specifiers supported by the printf family: `d i u x X o c s f e E g G %`, with
flags `- + space 0 #`, width, precision, and `*` for both (e.g. `%-10s`, `%08.3f`, `%.*g`).

### `<ctype.h>` → `ctype.hc`
| | |
|---|---|
| **Have** | `IsAlNum`, `IsAlpha`, `IsBlank`, `IsCntrl`, `IsDigit`, `IsGraph`, `IsLower`, `IsPrint`, `IsPunct`, `IsSpace`, `IsUpper`, `IsXDigit`, `ToLower`, `ToUpper` — a complete 1:1 set |
| **Different** | predicates return `0`/`1` (C returns nonzero/0); "C" locale only |

### `<math.h>` → `math.hc`
| | |
|---|---|
| **Have** | `Fabs`, `Sqrt`, `Cbrt`, `Hypot`, `Pow`, `Exp`/`Exp2`/`Expm1`, `Ln` (`log`)/`Log2`/`Log10`/`Log1p`, all trig + inverse + hyperbolic + inverse-hyperbolic, `Fmod`/`Remainder`/`FMA`/`Dim` (`fdim`), `Ceil`/`Floor`/`Trunc`/`Round`/`RoundToEven` (`rint`/`nearbyint`), `Frexp`/`Ldexp`/`Modf`/`Ilogb`/`Logb`/`Nextafter`/`Copysign`, classification `IsNaN`/`IsInf`/`Signbit`/`NaN`/`Inf`; the error/gamma family `Erf`/`Erfc`/`Gamma` (`tgamma`)/`Lgamma`; Bessel `J0`/`J1`/`Jn`/`Y0`/`Y1`/`Yn` (XSI) |
| **Missing** | `Fmax`/`Fmin` (see *Different*), `remquo`, `scalbn`/`scalbln`, `nexttoward`, `lround`/`llround`/`lrint`, `fpclassify`/`isfinite`/`isnormal`, the `f`/`l` suffixed variants (`sinf`/`sinl` — no `float`/`long double`) |
| **Different** | the generic `Min`/`Max` **return `I64`**, so `Min(3.0, 9.0)` truncates — there is no proper float `Fmax`/`Fmin`; `Mod`=`Fmod` and `Log`=`Ln` are aliases; transcendentals are *defined series* (reproducible bit-for-bit on every target), not "whatever libm does" |
| **Added** | `Erfinv`/`Erfcinv`, `Sincos`, `PowI` (exact integer power), `Pow10`, `Gcd`, `Factorial`, `Abs`/`Sign` (generic integer), `Float64bits`/`Float64frombits` (bit punning) |

### `<time.h>` → `time.hc`
| | |
|---|---|
| **Have** | `UnixNS` (wall clock, ns), `NanoNS` (monotonic, ns), `Sleep` (ns); calendar `DateTime`, `FromUnix`, `ToUnix`, `IsLeap`, `Now`, `FmtISO` |
| **Missing** | `localtime`/timezones (UTC only), `strftime` (only the fixed ISO format via `FmtISO`), `asctime`/`ctime`, `clock`/`CLOCKS_PER_SEC`, `difftime`, `mktime` with local tz |
| **Different** | nanosecond resolution throughout; `DateTime` is solomon's `struct tm` (UTC, `wday` filled) |
| **Added** | a true monotonic clock (`NanoNS`) distinct from the wall clock |

### `<threads.h>` (C11) → `threads.hc` + `<atomic.hc>`
| | |
|---|---|
| **Have** | `Thread` (`thrd_create`), `Join` (`thrd_join`); `Mutex` + `MutexInit`/`Lock`/`TryLock`/`Unlock` (`mtx_*`); `Cond` + `CondInit`/`Wait`/`Signal`/`Broadcast` (`cnd_*`) |
| **Missing** | `thrd_detach`, `thrd_yield`, `thrd_exit`, thread-local storage (`tss_*`), `call_once`/`ONCE_FLAG`, timed waits (`mtx_timedlock`, `cnd_timedwait`) |
| **Different** | a thread is `I64 (*)(I64)`; `Join` returns that `I64` result. No detach. The interpreter runs thread bodies synchronously |
| **Added** | `RwLock` (reader/writer lock, POSIX-style), and raw `FutexWait`/`FutexWake` |

### `<stdatomic.h>` → `atomic.hc`
| | |
|---|---|
| **Have** | `AtomicLoad`/`AtomicStore`/`AtomicAdd` (`fetch_add`)/`AtomicSwap` (`exchange`)/`AtomicCas` (`compare_exchange`); `AtomicFence` (`atomic_thread_fence`); `AtomicInc`/`AtomicDec` |
| **Missing** | `atomic_flag` + test-and-set, per-op `memory_order` arguments, the `_Atomic` type qualifier, narrow/`U128` atomics |
| **Different** | `I64`-only (C is type-generic); ordering is fixed (acquire/release on RMW, seq-cst fence) — you don't pass a `memory_order` |

### `<stdarg.h>` → language built-in
A `...` function reads its variadic slots through the sema-injected `VargC` (count) and
`VargV` (an `I64 *` of raw 8-byte slots) — there is no `va_list`/`va_start`/`va_arg`/`va_end`.

### `<stddef.h>` / `<stdint.h>` / `<stdbool.h>` → language built-in
`I8`/`U8`/`I16`/`U16`/`I32`/`U32`/`I64`/`U64`/`F64`/`Bool`/`U0` are **primitive types**
(not typedefs). `NULL`/`TRUE`/`FALSE` come from the prelude. `sizeof` and `offset`
(≈`offsetof`) are keywords. There is no `size_t`/`ptrdiff_t`/`intptr_t` name (use `I64`),
no `int_fast*`/`int_least*`, and `TRUE`/`FALSE` rather than `true`/`false`.

### Headers with no solomon equivalent
`<assert.h>` (no `assert` — use `throw`), `<errno.h>`, `<setjmp.h>` (use `try`/`catch`/
`throw` instead), `<signal.h>`, `<locale.h>`, `<float.h>`, `<limits.h>`, `<inttypes.h>`,
`<fenv.h>`, `<complex.h>`, `<tgmath.h>`, `<uchar.h>`, `<wchar.h>`, `<wctype.h>`,
`<stdalign.h>`, `<stdnoreturn.h>`, `<iso646.h>`.

---

## Beyond ISO C (POSIX-ish, present in solomon)

- **`<fcntl.hc>` / `<unistd.hc>`** — raw fd I/O (`Open`, `Read`, `Write`, `Close`,
  `LSeek`, `WriteAll`), process ids (`Getpid`/…), working dir (`Chdir`/`Getcwd`),
  `Mkdir`, and the portable `StdWrite`. Roughly POSIX `<unistd.h>`/`<fcntl.h>`.
- **`<socket.hc>`** — `Socket`/`Connect` plus high-level `ParseIPv4`, `MakeSockaddr`,
  `TcpConnect`, and a minimal `HttpGet`.

## solomon additions with no C analog at all

- **Generic containers**: `Vec<T>` (`<vec.hc>`, owning growable typed array) and
  `Hmap<K,V>` (`<hmap.hc>`, separate-chaining hash map). Fully typed, monomorphized at
  compile time — C has nothing like these.
- **Round-trip number↔string**: `F64ToStr` emits the shortest decimal that parses back to
  the exact same `F64`; `I64ToStr` is a direct integer formatter.
- **Errors as values** throughout (`-errno`), so no out-parameters and no `errno`.
- **A from-scratch correctly-rounded float formatter and `atof`**, identical bit-for-bit
  on the interpreter and every native backend (the freestanding targets have no libc).
