#ifndef _OS_HC
#define _OS_HC
// os.hc — process and OS helpers.
//
// Provides process control (`Exit`/`Getpid`/…), filesystem mutation
// (`Remove`/`Rename`/`Mkdir`), the working directory (`Chdir`/`Getcwd`), and the
// environment (`Getenv`/`Environ`). Include with `#include <os.hc>`. (The fd I/O
// `Open`/`Read`/`Write`/… and the higher-level file helpers stay in `<io.hc>`.)
//
// The primitives below are intrinsics: prototypes with no HolyC body. They are
// impure OS calls — `exit_group`/`exit`, `getpid`, `unlink`/`rename`/`mkdir` —
// lowered to the `*at` syscalls on the freestanding targets and to libc on Darwin,
// so the interpreter and backends each provide the lowering. `Getenv`/`Environ` are
// pure HolyC over the implicit `EnvP` array. `Environ` collects into a `<vec.hc>`
// Vec, hence the include.

#include <vec.hc>

// Terminate the process immediately with exit status `code` (its low 8 bits, per the
// OS convention). Does not return.
public U0 Exit(I64 code);

// Process / user ids.
public I64 Getpid();   // the current process id
public I64 Getppid();  // the parent process id
public I64 Getuid();   // the real user id
public I64 Getgid();   // the real group id

// Filesystem mutation. Each returns 0 on success, or a negative `-errno`.
public I64 Remove(U8 *path);                 // delete a file
public I64 Rename(U8 *oldpath, U8 *newpath); // rename/move
public I64 Mkdir(U8 *path, I64 mode);        // create a directory

// Working directory. `Chdir` changes it. `Getcwd` writes the current directory's
// path (NUL-terminated) into `buf` (capacity `size`). Each returns 0 on success, or
// `-errno` (e.g. `-ERANGE` if `buf` is too small).
public I64 Chdir(U8 *path);
public I64 Getcwd(U8 *buf, I64 size);

// Look up environment variable `name`. Returns a pointer to its value (the bytes
// after `name=` in the matching `EnvP` entry), or NULL if it is unset. The result
// points into the process environment, which is read-only: do not free or modify it.
public U8 *Getenv(U8 *name)
{
  if (EnvP == NULL) return NULL;   // no environment (e.g. Windows, for now)
  I64 i = 0;
  while (EnvP[i] != NULL) {
    U8 *e = EnvP[i];
    I64 j = 0;
    while (name[j] != 0 && e[j] == name[j]) j++;
    // The whole name matched and the entry's key ends exactly here ('='): a hit.
    if (name[j] == 0 && e[j] == '=') return e + j + 1;
    i++;
  }
  return NULL;
}

// Collect every environment entry ("KEY=VALUE", a `U8 *`) into `out`, a `Vec<U8 *>`
// initialised here, in the OS's order. Read an entry with `VecAt(&out, i)`. The
// entries point into the process environment and are read-only. `VecFree(&out)` frees
// the Vec's own buffer, not the entries.
public U0 Environ(Vec<U8 *> *out)
{
  VecInit(out);
  if (EnvP == NULL) return;   // no environment (e.g. Windows, for now)
  I64 i = 0;
  while (EnvP[i] != NULL) {
    VecPush(out, EnvP[i]);
    i++;
  }
}

#endif
