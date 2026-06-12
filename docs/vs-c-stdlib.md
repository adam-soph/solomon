# hcc stdlib vs. the C standard library

How hcc's HolyC standard library (`lib/*.hc`) relates to ISO C's. hcc mirrors
C's **header layout and grouping** (filenames follow the C headers), but keeps
**HolyC-PascalCase names** (`StrLen`, not `strlen`) and a few deliberate semantic
departures. Three buckets per area: **missing** (in C, not in hcc), **different**
(present but behaves unlike C), and **added** (in hcc, not in C). For the language-level
differences from TempleOS HolyC, see [`vs-holyc.md`](vs-holyc.md).

The quick name mapping: `malloc`→`MAlloc`, `free`→`Free`, `calloc`→`CAlloc`,
`realloc`→`ReAlloc`, `memcpy`→`MemCpy`, `strlen`→`StrLen`, `strstr`→`StrFind`,
`strtok`→`StrTok`/`StrTokR`, `printf`→`Print`, `fprintf`→`FPrint`, `sprintf`→`StrPrint`,
`snprintf`→`StrNPrint`, `scanf`→`Scan`, `sscanf`→`SScan`, `fputc`→`FPutC`, `fputs`→`FPutS`,
`qsort`→`Sort`, `bsearch`→`BSearch`, `atoi`→`StrToI64`, `strtol`→`StrToI64Base`,
`atof`→`StrToF64`, `strtod`→`StrToF64End`, `exit`→`Exit`, `_Exit`→`ExitRaw`,
`abort`→`Abort`, `atexit`→`AtExit`, `system`→`System`, `getenv`→`Getenv`,
`setenv`→`SetEnv`, `unsetenv`→`UnsetEnv`, `putenv`→`PutEnv`, `isdigit`→`IsDigit`,
`strerror`→`StrError`, `strftime`→`Strftime`, `strptime`→`StrPTime`, `mktime`→`MkTime`
(UTC), `asctime`→`AscTime`, `ctime`→`CTime`, `remquo`→`Remquo`, `scalbn`→`Scalbn`,
`call_once`→`CallOnce`, `thrd_yield`→`ThreadYield`, `thrd_exit`→`ThreadExit`,
`thrd_detach`→`ThreadDetach`, `mtx_timedlock`→`MutexTimedLock`,
`cnd_timedwait`→`CondTimedWait`, `tss_get`/`tss_set`→`TssGet`/`TssSet`.

---

## Cross-cutting differences (apply everywhere)

- **errno, both ways.** Failing OS calls return a negative `-errno` *as the value*
  (a non-negative result means success), **and** — like C — also record the positive
  code in the per-thread `errno` (a macro over `Fs->err`; `#include <errno.hc>`), which
  a successful call leaves untouched. Use whichever reads better: `if (fd == -ENOENT)`
  or `if (errno == ENOENT)`. The codes are Linux-canonical on every target (the Darwin
  backend + interpreter normalize the path-taking ops), so both forms are portable;
  the fd I/O and socket ops on Darwin/interpreter still return a plain `-1`, so prefer
  `errno` checks after `Open`/`Remove`/`Mkdir`/… there.
- **No `FILE *` / buffered streams.** I/O is file-descriptor based (`<unistd.hc>` /
  `<fcntl.hc>`) plus path-level helpers (`ReadFile`/`WriteFile`/…) and unbuffered char/line
  helpers (`FGetC`/`FGetS`/`GetLine`/`PutChar`/`Puts`). There is no `fopen`/`fread`/`fwrite`;
  `FPrint(fd, …)`/`FPutC`/`FPutS` are the `fprintf`/`fputc`/`fputs` analogs with the fd in
  place of the `FILE *`.
- **Formatted input is line-buffered.** `Scan` (scanf) streams from stdin — leftover
  input carries to the next call and a conversion list may span lines — reading whole
  lines underneath; `SScan` (sscanf) parses a caller's buffer. There is no `fscanf`
  over an arbitrary fd (read a line with `FGetS`/`ReadLine` and `SScan` it).
- **No wide chars, locales, or `float`.** Only the "C" locale, no `wchar_t`/`char16_t`, and
  the only floating type is `F64` (no `float`/`long double`, so no `sinf`/`sinl`).
- **Comparators normalize to `-1/0/1`** (`StrCmp`/`StrNCmp`/`MemCmp`), bytes compared *unsigned*.
- **Includes are explicit, like C — there is no auto-include.** The only ambient header is
  `<builtin.hc>` (auto-prepended: `NULL`/`TRUE`/`FALSE`, the `CTask` exception/errno type,
  and the implicit `argc`/`argv`/`envp`/`Fs`). Everything else needs its `#include`:
  `<stdlib.hc>` for `MAlloc`/`Free` and the rest of `<stdlib.h>`, `<stdio.hc>` to print
  (the `"fmt", …` form is a `Print` call) or scan, `<string.hc>` for the `Str*`/`Mem*`
  family, and so on. (A bare `"hi\n";` is the one exception — it lowers to a raw write and
  needs no include.) Broad includes are free: only functions reached from the entry point
  are emitted.

---

## Header-by-header

### `<string.h>` → `string.hc`
| | |
|---|---|
| **Have** | `StrLen`/`StrNLen`, `StrCmp`/`StrNCmp`, `StrCaseCmp`/`StrNCaseCmp`, `StrCpy`/`StrNCpy`, `StrCat`/`StrNCat`, `StrDup`/`StrNDup`, `StrFind` (`strstr`), `StrChr`, `StrLastChr` (`strrchr`), `StrSpn`/`StrCSpn`/`StrPBrk`, `StrTok`/`StrTokR`/`StrSep` (tokenizers); `MemCpy`, `MemMove`, `MemSet`, `MemCmp`, `MemFind` (`memchr`), `MemCCpy` (`memccpy`), `MemSearch` (`memmem`). `strerror`→`StrError` lives in `<errno.hc>` |
| **Missing** | `strcoll`/`strxfrm` (locale) |
| **Different** | `StrCmp`/`StrNCmp`/`MemCmp` are sign-normalized; bytes compare *unsigned* |
| **Added** | `StrInSet`, `StrToUpper`/`StrToLower`/`StrRev` (in-place), `CmpStr` (ready comparator) |

### `<stdlib.h>` → `stdlib.hc`
| | |
|---|---|
| **Have** | `MAlloc`, `Free`, `CAlloc`, `ReAlloc`; `StrToI64` (`atoi`/`atoll`), `StrToI64Base` (`strtol`, base+endptr), `StrToU64Base` (`strtoul`), `StrToF64` (`atof`), `StrToF64End` (`strtod`, endptr); `Div` (`div`/`ldiv`, tuple); `Sort` (`qsort`), `BSearch` (`bsearch`); `RandU64`/`SeedRand`; `Exit`, `ExitRaw` (`_Exit`), `Abort` (`abort`), `AtExit` (`atexit`, ≥32 slots, LIFO); `System` (`system`); `Getenv`, `SetEnv`/`UnsetEnv`/`PutEnv` |
| **Missing** | `quick_exit`/`at_quick_exit`, `aligned_alloc`, the multibyte (`mbtowc` …) family, `RAND_MAX` |
| **Different** | `RandU64` returns a full 64-bit value (not `[0, RAND_MAX]`); `Sort`/`BSearch` are **typed generics** (`Sort<T>`), not `void*` + element-size; `abs` lives in `<math.hc>`; `Div` returns a tuple, not a `div_t` struct; `Abort` exits 134 (no signal machinery); `SetEnv` writes an override table `Getenv` consults — the process environment itself is untouched, so a `System` child doesn't see it; freestanding `System` children get an empty environment, and the Windows native target rejects `System` at compile time for now |
| **Added** | `MSize`, `HeapExtend`, `I64ToStr`, `F64ToStr` (shortest round-trip), comparators `CmpI64`/`CmpU64`/`CmpF64`; `Environ` (collect the whole environment into a `Vec<U8*>`, in `<vec.hc>`) |

### `<stdio.h>` → `stdio.hc`
| | |
|---|---|
| **Have** | `Print` (`printf`), `FPrint` (`fprintf`, fd-based), `StrPrint` (`sprintf`), `StrNPrint` (`snprintf`), `MStrPrint` (`asprintf`), `CatPrint`; `Scan` (`scanf`, streaming stdin), `SScan` (`sscanf`); `FGetC`/`GetChar` (`fgetc`/`getchar`), `FGetS` (`fgets`), `GetLine`, `ReadLine`; `PutChar` (`putchar`), `Puts` (`puts`), `FPutC`/`FPutS` (`fputc`/`fputs`, fd-based); `Remove`/`Rename`; path helpers `ReadFile`/`WriteFile`/`AppendFile`/`FileSize`. (`perror`→`Perror` in `<errno.hc>`) |
| **Missing** | **The `FILE *` layer**: `fopen`/`fclose`/`fread`/`fwrite`/`fseek`/`ftell`/`fflush`/`feof`/`ferror`/`setvbuf`/`ungetc`/`tmpfile` (deliberate — fd I/O instead). `fscanf` over an arbitrary fd (`Scan` is stdin-only); `vprintf` (no `va_list` — forward `argc`/`argv` instead) |
| **Different** | `StrPrint` is **unbounded** (use `StrNPrint` for a size bound); `Scan` is line-buffered (leftover input carries between calls; conversions span lines but never split mid-line token); `SScan`/`Scan`'s `%f` is a direct (not the correctly-rounded `StrToF64`) parser; float formatting is hcc's own correctly-rounded formatter (`FmtFloat`, also callable directly), byte-identical across interpreter and every backend |
| **Added** | `MStrPrint`, `CatPrint`, `ReadLine`, the path file helpers; portable `StdWrite` (in `<unistd.hc>`, works on Windows) |

Printf specifiers: `d i u x X o c s f e E g G %`, flags `- + space 0 #`, width, precision,
and `*` (e.g. `%-10s`, `%08.3f`, `%.*g`). `SScan`/`Scan` mirror the same conversions + `*`
suppression + width (length modifiers `l h L z j t` are accepted and ignored — HolyC is
uniform-width).

### `<ctype.h>` → `ctype.hc`
| | |
|---|---|
| **Have** | `IsAlNum`/`IsAlpha`/`IsBlank`/`IsCntrl`/`IsDigit`/`IsGraph`/`IsLower`/`IsPrint`/`IsPunct`/`IsSpace`/`IsUpper`/`IsXDigit`/`ToLower`/`ToUpper` — a complete 1:1 set |
| **Different** | predicates return `0`/`1`; "C" locale only |

### `<math.h>` → `math.hc`
| | |
|---|---|
| **Have** | `Fabs`/`Sqrt`/`Cbrt`/`Hypot`/`Pow`, `Exp`/`Exp2`/`Expm1`, `Ln`/`Log2`/`Log10`/`Log1p`, all trig+inverse+hyperbolic+inverse-hyperbolic, `Fmod`/`Remainder`/`FMA`/`Dim` (`fdim`), `Fmin`/`Fmax`, `Ceil`/`Floor`/`Trunc`/`Round`/`RoundToEven` (`rint`/`nearbyint`), `LRound`/`LLRound`/`LRint` (`lround`/`llround`/`lrint`), `Frexp`/`Ldexp`/`Modf`/`Ilogb`/`Logb`/`Nextafter`/`Copysign`; classification `FpClassify`/`IsFinite`/`IsNormal`/`IsNaN`/`IsInf`/`Signbit`/`NaN`/`Inf` (+ `FP_*`); error/gamma `Erf`/`Erfc`/`Gamma` (`tgamma`)/`Lgamma`; Bessel `J0`/`J1`/`Jn`/`Y0`/`Y1`/`Yn` |
| **Missing** | the `f`/`l` suffixed variants (no `float`/`long double`) — the double set is complete (`Remquo`, `Scalbn`/`Scalbln` = `Ldexp`, `Nexttoward` = `Nextafter` included) |
| **Different** | generic `Min`/`Max`/`Abs` return the element type `T` (float-correct, with `fmin`/`fmax` NaN semantics); `Mod`=`Fmod`, `Log`=`Ln` aliases; transcendentals are *defined series* (reproducible bit-for-bit), not "whatever libm does" |
| **Added** | `Erfinv`/`Erfcinv`, `Sincos`, `PowI`, `Pow10`, `Gcd`, `Factorial`, `Sign`, `Float64bits`/`Float64frombits` |

### `<time.h>` → `time.hc`
| | |
|---|---|
| **Have** | `UnixNS` (wall, ns), `NanoNS` (monotonic, ns), `CpuNS`/`Clock` (process CPU time) + `CLOCKS_PER_SEC`, `Sleep`; `Difftime`; calendar `DateTime`, `FromUnix` (`gmtime`), `ToUnix` (`timegm`), `MkTime` (`mktime`-style field normalization, UTC), `StrPTime` (`strptime`), `AscTime`/`CTime` (`asctime`/`ctime`), `Localtime` (fixed-offset), `IsLeap`, `Now`, `FmtISO`, `Strftime` |
| **Missing** | tz-database `localtime` (the offset is explicit — `MkTime`/`CTime` are UTC, C's are local); `%Z`/`%z` in `StrPTime` |
| **Different** | nanosecond resolution; `DateTime` is hcc's `struct tm` (UTC, `wday` filled); `MkTime` takes a `DateTime *` and rewrites it normalized |
| **Added** | a true monotonic clock (`NanoNS`) distinct from the wall clock |

### `<errno.h>` → `errno.hc`
| | |
|---|---|
| **Have** | a real per-thread **`errno`** (a macro over `Fs->err`, set by failing primitives, assignable, untouched on success) + `Errno()`; ~85 named codes (`EPERM`…`ENOTRECOVERABLE`); `StrError` (`strerror`), `Perror` (`perror`, takes the code explicitly — pass `errno` for the C form) |
| **Missing** | a handful of exotic kernel-internal codes (`ECHRNG`, `EBADE`, … — add on demand) |
| **Different** | codes are Linux-canonical and normalized on every target; failing calls *also* return `-errno` as the value, so both styles work; the Darwin/interpreter fd+socket ops return a plain `-1` (prefer `errno` after path-taking ops there) |

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
| **Have** | `Thread` (`thrd_create`), `Join` (`thrd_join`), `ThreadDetach` (`thrd_detach`), `ThreadYield` (`thrd_yield`), `ThreadExit` (`thrd_exit`); `Mutex` + `MutexInit`/`Lock`/`TryLock`/`TimedLock`/`Unlock`; `Cond` + `CondInit`/`Wait`/`TimedWait`/`Signal`/`Broadcast`; `Once` + `OnceInit`/`CallOnce` (`call_once`); TLS `TssCreate`/`TssSet`/`TssGet` (`tss_*`) |
| **Missing** | TLS destructors (`tss_create`'s dtor argument — a thread's entries are abandoned at exit) |
| **Different** | a thread is `I64 (*)(I64)`, `Join` returns that `I64` (`ThreadExit(ret)` supplies the same value early; from the main flow it exits the program); the timed waits take a relative nanosecond timeout, not an absolute `timespec`; freestanding `ThreadDetach` is a documented no-op (clone(2) thread stacks are never reclaimed); the interpreter runs bodies synchronously; the native Windows target has no thread spawn yet — `Thread`/`Join`/the futex ops are compile errors there, though `ThreadYield`/`Gettid` work (kernel32) |
| **Added** | `RwLock` (reader/writer lock), raw `FutexWait`/`FutexWake`/`FutexWaitNs` |

### `<stdatomic.h>` → `stdatomic.hc`
| | |
|---|---|
| **Have** | `AtomicLoad`/`AtomicStore`/`AtomicAdd` (`fetch_add`)/`AtomicSwap` (`exchange`)/`AtomicCas` (`compare_exchange`); `AtomicAnd`/`AtomicOr`/`AtomicXor` (`fetch_and`/`or`/`xor`, CAS loops); `AtomicFlagTestAndSet`/`AtomicFlagClear` (`atomic_flag`); `AtomicFence`; `AtomicInc`/`AtomicDec` |
| **Missing** | per-op `memory_order` args, the `_Atomic` qualifier, `U128` atomics |
| **Different** | the bitwise RMW ops return the OLD value like C's `fetch_*`, but `AtomicAdd` (which predates them) returns the NEW value; ordering fixed (acquire/release on RMW, seq-cst fence) |

### `<stdarg.h>` → language built-in
A `...` function reads its variadic slots through the sema-injected `argc` (count) and
`argv` (an `I64 *` of raw 8-byte slots) — no `va_list`/`va_start`/`va_arg`/`va_end`. These
are the same `argc`/`argv` names that mean the command line at top-level scope; inside a
`...` function they are the varargs instead, and inside a non-variadic function neither.

### `<stddef.h>` / `<stdint.h>` / `<stdbool.h>` → language built-in
`I8`…`U64`/`F64`/`Bool`/`U0` are **primitive types** (not typedefs). `NULL`/`TRUE`/`FALSE`
from the prelude. `sizeof` and `offset` (≈`offsetof`) are keywords. No `size_t`/`ptrdiff_t`
name (use `I64`); `TRUE`/`FALSE` rather than `true`/`false`.

### Headers with no hcc equivalent
`<assert.h>` (use `throw`), `<setjmp.h>` (use `try`/`catch`/`throw`), `<signal.h>`,
`<locale.h>`, `<complex.h>`, `<tgmath.h>`, `<fenv.h>`, `<uchar.h>`, `<wchar.h>`,
`<wctype.h>`, `<stdalign.h>`, `<stdnoreturn.h>`, `<iso646.h>`, `<inttypes.h>`.

---

## Beyond ISO C (POSIX-ish / platform, present in hcc)

- **`<fcntl.hc>` / `<unistd.hc>`** — raw fd I/O (`Open`, `Read`, `Write`, `Close`, `LSeek`,
  `WriteAll`), process/thread ids (`Getpid`/`Gettid`/…), working dir (`Chdir`/`Getcwd`),
  `Mkdir`, portable `StdWrite`. Missing the broader POSIX surface
  (`dup`/`pipe`/`fork`/`exec`/`stat`/… — though `System` covers the run-a-command case).
- **`<socket.hc>`** — client TCP: `Socket`/`Connect` + `ParseIPv4`/`MakeSockaddr`/
  `TcpConnect`/`HttpGet`. No server side (`bind`/`listen`/`accept`/`send`/`recv`).
- **`<windows.hc>` (Windows-only, gated on `_WIN32`)** — hand-built `kernel32`/`advapi32`
  imports for the self-contained PE target: file I/O (`CreateFileA`/`ReadFile`/`WriteFile`/
  `CloseHandle`/`SetFilePointerEx`/`GetFileSizeEx`), misc queries (`GetLastError`/
  `GetCurrentProcessId`), and registry access (`RegCreateKeyExA`/`RegSetValueExA`/
  `RegQueryValueExA`/`RegCloseKey`/`RegDeleteKeyA`). No POSIX equivalent; absent on the
  other targets.

## hcc additions with no C analog

- **Generic containers**: `Vec<T>` (`<vec.hc>`) and `Hmap<K,V>` (`<hmap.hc>`), fully typed,
  monomorphized at compile time.
- **Round-trip number↔string**: `F64ToStr` emits the shortest decimal that parses back to
  the exact `F64`.
- **Errors as values** throughout (`-errno` returns, alongside the `errno` global), so no
  out-parameters.
- **A from-scratch correctly-rounded float formatter and `atof`/`strtod`**, identical
  bit-for-bit on the interpreter and every native backend (the freestanding targets have no libc).

## Residual gaps, deliberately

- **No `FILE *`** — the one C mechanism left out on purpose; fd I/O + the path helpers
  cover the use cases without a buffering layer.
- **`System` on the native Windows target** is a compile-time error for now (the
  interpreter models it with `cmd /C` on a Windows host); the future path is a pure-HolyC
  `System` in `<windows.hc>` over `CreateProcessA` imports.
- **Freestanding thread stacks are never reclaimed** (`ThreadDetach` there only forfeits
  the join), and **TLS has no destructors** — both bounded leaks, documented in
  `<threads.hc>`.
- **`assert`** stays a pattern (`if (!ok) throw 'ASRT';`): the preprocessor has no `#`
  stringification or `__FILE__`/`__LINE__`, so a C-faithful assert macro can't exist yet.
