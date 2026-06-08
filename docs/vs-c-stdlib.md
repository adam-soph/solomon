# solomon stdlib vs. the C standard library

How solomon's HolyC standard library (`lib/*.hc`) relates to ISO C's. solomon mirrors
C's **header layout and grouping** (filenames follow the C headers), but keeps
**HolyC-PascalCase names** (`StrLen`, not `strlen`) and a few deliberate semantic
departures. Three buckets per area: **missing** (in C, not in solomon), **different**
(present but behaves unlike C), and **added** (in solomon, not in C).

The quick name mapping: `malloc`→`MAlloc`, `free`→`Free`, `calloc`→`CAlloc`,
`realloc`→`ReAlloc`, `memcpy`→`MemCpy`, `strlen`→`StrLen`, `strstr`→`StrFind`,
`strtok`→`StrTok`/`StrTokR`, `printf`→`Print`, `sprintf`→`StrPrint`, `snprintf`→`StrNPrint`,
`sscanf`→`SScan`, `qsort`→`Sort`, `bsearch`→`BSearch`, `atoi`→`StrToI64`,
`strtol`→`StrToI64Base`, `atof`→`StrToF64`, `strtod`→`StrToF64End`, `exit`→`Exit`,
`getenv`→`Getenv`, `isdigit`→`IsDigit`, `strerror`→`StrError`, `strftime`→`Strftime`.

---

## Cross-cutting differences (apply everywhere)

- **`errno` as a return value.** Failing OS calls return a negative `-errno` *as the value*
  (a non-negative result means success). There is no `errno` global, but `<errno.hc>` has
  the named codes (`ENOENT`, …) and `StrError`/`Perror`. The codes are Linux-canonical on
  every target (the Darwin backend + interpreter normalize), so `ret == -ENOENT` is portable.
- **No `FILE *` / buffered streams.** I/O is file-descriptor based (`<unistd.hc>` /
  `<fcntl.hc>`) plus path-level helpers (`ReadFile`/`WriteFile`/…) and unbuffered char/line
  helpers (`FGetC`/`FGetS`/`GetLine`/`PutChar`/`Puts`). There is no `fopen`/`fread`/`fwrite`.
- **Formatted input is buffer-based.** `SScan` (sscanf) parses a string; for stdin, read a
  line with `FGetS`/`ReadLine` and `SScan` it. There is no streaming `scanf`/`fscanf`.
- **No wide chars, locales, or `float`.** Only the "C" locale, no `wchar_t`/`char16_t`, and
  the only floating type is `F64` (no `float`/`long double`, so no `sinf`/`sinl`).
- **Comparators normalize to `-1/0/1`** (`StrCmp`/`StrNCmp`/`MemCmp`), bytes compared *unsigned*.
- **`MAlloc`/`Free` are always in scope** (the `<builtin.hc>` prelude); the rest of
  `<stdlib.h>` needs `#include <stdlib.hc>`. Printing / input / scan auto-include `<stdio.hc>`.

---

## Header-by-header

### `<string.h>` → `string.hc`
| | |
|---|---|
| **Have** | `StrLen`/`StrNLen`, `StrCmp`/`StrNCmp`, `StrCaseCmp`/`StrNCaseCmp`, `StrCpy`/`StrNCpy`, `StrCat`/`StrNCat`, `StrDup`/`StrNDup`, `StrFind` (`strstr`), `StrChr`, `StrLastChr` (`strrchr`), `StrSpn`/`StrCSpn`/`StrPBrk`, `StrTok`/`StrTokR`/`StrSep` (tokenizers); `MemCpy`, `MemMove`, `MemSet`, `MemCmp`, `MemFind` (`memchr`), `MemCCpy` (`memccpy`), `MemSearch` (`memmem`). `strerror`→`StrError` lives in `<errno.hc>` |
| **Missing** | `strcoll`/`strxfrm` (locale) |
| **Different** | `StrCmp`/`StrNCmp`/`MemCmp` are sign-normalized; bytes compare *unsigned* |
| **Added** | `StrInSet`, `StrToUpper`/`StrToLower`/`StrRev` (in-place), `CmpStr` (ready comparator) |

### `<stdlib.h>` → `stdlib.hc` (+ `MAlloc`/`Free` in the prelude)
| | |
|---|---|
| **Have** | `MAlloc`, `Free`, `CAlloc`, `ReAlloc`; `StrToI64` (`atoi`/`atoll`), `StrToI64Base` (`strtol`, base+endptr), `StrToU64Base` (`strtoul`), `StrToF64` (`atof`), `StrToF64End` (`strtod`, endptr); `Div` (`div`/`ldiv`, tuple); `Sort` (`qsort`), `BSearch` (`bsearch`); `RandU64`/`SeedRand`; `Exit`; `Getenv` |
| **Missing** | `abort`, `_Exit`, `atexit`/`quick_exit`, `system`, `setenv`/`putenv`/`unsetenv`, `aligned_alloc`, the multibyte (`mbtowc` …) family, `RAND_MAX` |
| **Different** | `RandU64` returns a full 64-bit value (not `[0, RAND_MAX]`); `Sort`/`BSearch` are **typed generics** (`Sort<T>`), not `void*` + element-size; `abs` lives in `<math.hc>`; `Div` returns a tuple, not a `div_t` struct |
| **Added** | `MSize`, `HeapExtend`, `I64ToStr`, `F64ToStr` (shortest round-trip), comparators `CmpI64`/`CmpU64`/`CmpF64` |

### `<stdio.h>` → `stdio.hc`
| | |
|---|---|
| **Have** | `Print` (`printf`), `StrPrint` (`sprintf`), `StrNPrint` (`snprintf`), `MStrPrint` (`asprintf`), `CatPrint`; `SScan` (`sscanf`); `FGetC`/`GetChar` (`fgetc`/`getchar`), `FGetS` (`fgets`), `GetLine`, `ReadLine`; `PutChar` (`putchar`), `Puts` (`puts`); `Remove`/`Rename`; path helpers `ReadFile`/`WriteFile`/`AppendFile`/`FileSize`. (`perror`→`Perror` in `<errno.hc>`) |
| **Missing** | **The `FILE *` layer**: `fopen`/`fclose`/`fread`/`fwrite`/`fseek`/`ftell`/`fflush`/`feof`/`ferror`/`setvbuf`/`ungetc`/`tmpfile`. **Streaming input**: `scanf`/`fscanf` (have `SScan`). `fprintf`/`vprintf` (no FILE/va_list), `fputs`/`fputc` to an arbitrary fd |
| **Different** | `StrPrint` is **unbounded** (use `StrNPrint` for a size bound); `SScan`'s `%f` is a direct (not the correctly-rounded `StrToF64`) parser; float formatting is solomon's own correctly-rounded formatter, byte-identical across interpreter and every backend |
| **Added** | `MStrPrint`, `CatPrint`, `ReadLine`, the path file helpers; portable `StdWrite` (in `<unistd.hc>`, works on Windows) |

Printf specifiers: `d i u x X o c s f e E g G %`, flags `- + space 0 #`, width, precision,
and `*` (e.g. `%-10s`, `%08.3f`, `%.*g`). `SScan` mirrors the same conversions + `*` suppression + width.

### `<ctype.h>` → `ctype.hc`
| | |
|---|---|
| **Have** | `IsAlNum`/`IsAlpha`/`IsBlank`/`IsCntrl`/`IsDigit`/`IsGraph`/`IsLower`/`IsPrint`/`IsPunct`/`IsSpace`/`IsUpper`/`IsXDigit`/`ToLower`/`ToUpper` — a complete 1:1 set |
| **Different** | predicates return `0`/`1`; "C" locale only |

### `<math.h>` → `math.hc`
| | |
|---|---|
| **Have** | `Fabs`/`Sqrt`/`Cbrt`/`Hypot`/`Pow`, `Exp`/`Exp2`/`Expm1`, `Ln`/`Log2`/`Log10`/`Log1p`, all trig+inverse+hyperbolic+inverse-hyperbolic, `Fmod`/`Remainder`/`FMA`/`Dim` (`fdim`), `Fmin`/`Fmax`, `Ceil`/`Floor`/`Trunc`/`Round`/`RoundToEven` (`rint`/`nearbyint`), `LRound`/`LLRound`/`LRint` (`lround`/`llround`/`lrint`), `Frexp`/`Ldexp`/`Modf`/`Ilogb`/`Logb`/`Nextafter`/`Copysign`; classification `FpClassify`/`IsFinite`/`IsNormal`/`IsNaN`/`IsInf`/`Signbit`/`NaN`/`Inf` (+ `FP_*`); error/gamma `Erf`/`Erfc`/`Gamma` (`tgamma`)/`Lgamma`; Bessel `J0`/`J1`/`Jn`/`Y0`/`Y1`/`Yn` |
| **Missing** | `remquo`, `scalbn`/`scalbln` (named — `Ldexp` is it), `nexttoward`; the `f`/`l` suffixed variants (no `float`/`long double`) |
| **Different** | generic `Min`/`Max`/`Abs` return the element type `T` (float-correct, with `fmin`/`fmax` NaN semantics); `Mod`=`Fmod`, `Log`=`Ln` aliases; transcendentals are *defined series* (reproducible bit-for-bit), not "whatever libm does" |
| **Added** | `Erfinv`/`Erfcinv`, `Sincos`, `PowI`, `Pow10`, `Gcd`, `Factorial`, `Sign`, `Float64bits`/`Float64frombits` |

### `<time.h>` → `time.hc`
| | |
|---|---|
| **Have** | `UnixNS` (wall, ns), `NanoNS` (monotonic, ns), `CpuNS`/`Clock` (process CPU time) + `CLOCKS_PER_SEC`, `Sleep`; `Difftime`; calendar `DateTime`, `FromUnix` (`gmtime`), `ToUnix` (`timegm`), `Localtime` (fixed-offset), `IsLeap`, `Now`, `FmtISO`, `Strftime` |
| **Missing** | `strptime` (parse), `mktime` field-normalization (`ToUnix` assumes valid fields), tz-database `localtime` (offset is explicit), `asctime`/`ctime` (named — `Strftime "%c"` is the form) |
| **Different** | nanosecond resolution; `DateTime` is solomon's `struct tm` (UTC, `wday` filled) |
| **Added** | a true monotonic clock (`NanoNS`) distinct from the wall clock |

### `<errno.h>` → `errno.hc`
| | |
|---|---|
| **Have** | ~60 named codes (`EPERM`…`ECANCELED`), `StrError` (`strerror`), `Perror` (`perror`, takes the code explicitly) |
| **Missing** | the full POSIX code set (the common file/process/socket ones are present); no `errno` global (codes are returned as `-errno`) |
| **Different** | codes are Linux-canonical and normalized on every target; `Perror(msg, err)` takes the error explicitly (no global) |

### `<limits.h>` / `<stdint.h>` (limits) → `limits.hc`
| | |
|---|---|
| **Have** | `CHAR_BIT`, `I8_MIN`/`I8_MAX`/`U8_MAX` … `I64_MIN`/`I64_MAX`/`U64_MAX` |
| **Different** | explicit-width names (`I64_MAX`, not `INT_MAX`/`LONG_MAX`, which would be ambiguous — HolyC's `int` is 64-bit) |

### `<float.h>` → `float.hc`
| | |
|---|---|
| **Have** | `FLT_RADIX`, `F64_MANT_DIG`/`DIG`/`MIN_EXP`/`MAX_EXP`/`MIN_10_EXP`/`MAX_10_EXP`, `F64_MAX`/`F64_MIN`/`F64_EPSILON`/`F64_TRUE_MIN` (+ `DBL_*` aliases) |
| **Missing** | `DECIMAL_DIG`, `FLT_ROUNDS`/`FLT_EVAL_METHOD`, `FLT_*`/`LDBL_*` (no `float`/long double) |

### `<threads.h>` (C11) → `threads.hc` + `<stdatomic.hc>`
| | |
|---|---|
| **Have** | `Thread` (`thrd_create`), `Join` (`thrd_join`); `Mutex` + `MutexInit`/`Lock`/`TryLock`/`Unlock`; `Cond` + `CondInit`/`Wait`/`Signal`/`Broadcast` |
| **Missing** | `thrd_detach`/`thrd_yield`/`thrd_exit`, TLS (`tss_*`), `call_once`, timed waits |
| **Different** | a thread is `I64 (*)(I64)`, `Join` returns that `I64`; the interpreter runs bodies synchronously |
| **Added** | `RwLock` (reader/writer lock), raw `FutexWait`/`FutexWake` |

### `<stdatomic.h>` → `stdatomic.hc`
| | |
|---|---|
| **Have** | `AtomicLoad`/`AtomicStore`/`AtomicAdd` (`fetch_add`)/`AtomicSwap` (`exchange`)/`AtomicCas` (`compare_exchange`); `AtomicFence`; `AtomicInc`/`AtomicDec` |
| **Missing** | `atomic_flag` + test-and-set, per-op `memory_order` args, `fetch_and`/`or`/`xor`, the `_Atomic` qualifier, narrow/`U128` atomics |
| **Different** | `I64`-only (C is type-generic); ordering fixed (acquire/release on RMW, seq-cst fence) |

### `<stdarg.h>` → language built-in
A `...` function reads its variadic slots through the sema-injected `VargC` (count) and
`VargV` (an `I64 *` of raw 8-byte slots) — no `va_list`/`va_start`/`va_arg`/`va_end`.

### `<stddef.h>` / `<stdint.h>` / `<stdbool.h>` → language built-in
`I8`…`U64`/`F64`/`Bool`/`U0` are **primitive types** (not typedefs). `NULL`/`TRUE`/`FALSE`
from the prelude. `sizeof` and `offset` (≈`offsetof`) are keywords. No `size_t`/`ptrdiff_t`
name (use `I64`); `TRUE`/`FALSE` rather than `true`/`false`.

### Headers with no solomon equivalent
`<assert.h>` (use `throw`), `<setjmp.h>` (use `try`/`catch`/`throw`), `<signal.h>`,
`<locale.h>`, `<complex.h>`, `<tgmath.h>`, `<fenv.h>`, `<uchar.h>`, `<wchar.h>`,
`<wctype.h>`, `<stdalign.h>`, `<stdnoreturn.h>`, `<iso646.h>`, `<inttypes.h>`.

---

## Beyond ISO C (POSIX-ish, present in solomon)

- **`<fcntl.hc>` / `<unistd.hc>`** — raw fd I/O (`Open`, `Read`, `Write`, `Close`, `LSeek`,
  `WriteAll`), process ids (`Getpid`/…), working dir (`Chdir`/`Getcwd`), `Mkdir`, portable
  `StdWrite`. Missing the broader POSIX surface (`dup`/`pipe`/`fork`/`exec`/`stat`/…).
- **`<socket.hc>`** — client TCP: `Socket`/`Connect` + `ParseIPv4`/`MakeSockaddr`/
  `TcpConnect`/`HttpGet`. No server side (`bind`/`listen`/`accept`/`send`/`recv`).

## solomon additions with no C analog

- **Generic containers**: `Vec<T>` (`<vec.hc>`) and `Hmap<K,V>` (`<hmap.hc>`), fully typed,
  monomorphized at compile time.
- **Round-trip number↔string**: `F64ToStr` emits the shortest decimal that parses back to
  the exact `F64`.
- **Errors as values** throughout (`-errno`), so no out-parameters.
- **A from-scratch correctly-rounded float formatter and `atof`/`strtod`**, identical
  bit-for-bit on the interpreter and every native backend (the freestanding targets have no libc).
