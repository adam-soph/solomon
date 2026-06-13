#ifndef _ERRNO_HH
#define _ERRNO_HH
// errno.hh — C `<errno.h>`: the `errno` variable, named error codes, plus
// `StrError`/`Perror` to render them. The stdlib's OS primitives
// (`Open`/`Read`/`Write`/`Close`/`LSeek`/`Socket`/`Connect`/`Mkdir`/`Remove`/
// `Rename`/`Chdir`/`Getcwd`/…) report failure Unix-style: a non-negative result on
// success, a negative `-errno` on error — and, like C, a failing call also records
// the positive code in the per-thread `errno` (the compiler emits the store into
// `Fs->err`; success leaves it unchanged). Use whichever reads better:
// `if (fd == -ENOENT)` on the return value, or `if (errno == ENOENT)` after it.
// Include with `#include <errno.hc>`.
//
// The values are **Linux-canonical** and portable: the freestanding Linux targets
// return them directly, and the Darwin backend + interpreter normalise the host's
// native errno to these numbers, so `if (ret == -ENOENT)` behaves the same on every
// target. (Windows file ops currently surface a plain `-1`, so `StrError(-1)` is the
// generic "Unknown error" there until per-error mapping lands. The fd I/O and socket
// ops on Darwin/interpreter also return a plain `-1` today, so prefer testing errno
// after the path-taking calls — `Open`/`Remove`/`Mkdir`/… — which are normalised
// everywhere.)

// C's `errno`, as a macro over the per-thread task context (`<builtin.hc>`'s
// `CTask *Fs`), so plain C idioms compile: `errno = 0; ...; if (errno) ...`.
// It is assignable, like C's.
#define errno (Fs->err)

// Function form of the same read, for when a macro is awkward.
public I64 Errno();

// --- error codes (Linux numbering) -------------------------------------------

#define EPERM         1   // Operation not permitted
#define ENOENT        2   // No such file or directory
#define EINTR         4   // Interrupted system call
#define EIO           5   // Input/output error
#define EBADF         9   // Bad file descriptor
#define EAGAIN       11   // Resource temporarily unavailable
#define EWOULDBLOCK  11   // (alias of EAGAIN)
#define ENOMEM       12   // Cannot allocate memory
#define EACCES       13   // Permission denied
#define EFAULT       14   // Bad address
#define EBUSY        16   // Device or resource busy
#define EEXIST       17   // File exists
#define EXDEV        18   // Invalid cross-device link
#define ENODEV       19   // No such device
#define ENOTDIR      20   // Not a directory
#define EISDIR       21   // Is a directory
#define EINVAL       22   // Invalid argument
#define ENFILE       23   // Too many open files in system
#define EMFILE       24   // Too many open files
#define EFBIG        27   // File too large
#define ENOSPC       28   // No space left on device
#define ESPIPE       29   // Illegal seek
#define EROFS        30   // Read-only file system
#define EPIPE        32   // Broken pipe
#define ERANGE       34   // Numerical result out of range
#define ENAMETOOLONG 36   // File name too long
#define ENOTEMPTY    39   // Directory not empty
#define ELOOP        40   // Too many levels of symbolic links
#define EADDRINUSE   98   // Address already in use
#define ENETUNREACH 101   // Network is unreachable
#define ECONNRESET  104   // Connection reset by peer
#define ETIMEDOUT   110   // Connection timed out
#define ECONNREFUSED 111  // Connection refused
#define EHOSTUNREACH 113  // No route to host
#define EINPROGRESS 115   // Operation now in progress

#define ESRCH           3 // No such process
#define ENXIO           6 // No such device or address
#define E2BIG           7 // Argument list too long
#define ENOEXEC         8 // Exec format error
#define ECHILD         10 // No child processes
#define ETXTBSY        26 // Text file busy
#define EMLINK         31 // Too many links
#define EDOM           33 // Numerical argument out of domain
#define EDEADLK        35 // Resource deadlock avoided
#define ENOSYS         38 // Function not implemented
#define ENOTSOCK       88 // Socket operation on non-socket
#define EDESTADDRREQ   89 // Destination address required
#define EMSGSIZE       90 // Message too long
#define EPROTOTYPE     91 // Protocol wrong type for socket
#define ENOPROTOOPT    92 // Protocol not available
#define EPROTONOSUPPORT 93 // Protocol not supported
#define EOPNOTSUPP     95 // Operation not supported
#define EAFNOSUPPORT   97 // Address family not supported by protocol
#define EADDRNOTAVAIL  99 // Cannot assign requested address
#define ENETDOWN      100 // Network is down
#define ECONNABORTED  103 // Software caused connection abort
#define ENOBUFS       105 // No buffer space available
#define EISCONN       106 // Transport endpoint is already connected
#define ENOTCONN      107 // Transport endpoint is not connected
#define EHOSTDOWN     112 // Host is down
#define EALREADY      114 // Operation already in progress
#define ECANCELED     125 // Operation canceled

#define ENOTBLK        15 // Block device required
#define ENOLCK         37 // No locks available
#define ENOMSG         42 // No message of desired type
#define EIDRM          43 // Identifier removed
#define ENOSTR         60 // Device not a stream
#define ENODATA        61 // No data available
#define ETIME          62 // Timer expired
#define ENOSR          63 // Out of streams resources
#define ENOLINK        67 // Link has been severed
#define EPROTO         71 // Protocol error
#define EMULTIHOP      72 // Multihop attempted
#define EBADMSG        74 // Bad message
#define EOVERFLOW      75 // Value too large for defined data type
#define EILSEQ         84 // Invalid or incomplete multibyte or wide character
#define EUSERS         87 // Too many users
#define ESOCKTNOSUPPORT 94 // Socket type not supported
#define EPFNOSUPPORT   96 // Protocol family not supported
#define ENETRESET     102 // Network dropped connection on reset
#define ESHUTDOWN     108 // Cannot send after transport endpoint shutdown
#define ETOOMANYREFS  109 // Too many references: cannot splice
#define ESTALE        116 // Stale file handle
#define EDQUOT        122 // Disk quota exceeded
#define EOWNERDEAD    130 // Owner died
#define ENOTRECOVERABLE 131 // State not recoverable

// --- rendering ---------------------------------------------------------------

// The message for an errno, like C's `strerror`. Accepts either the positive code
// or the negative `-errno` returned by the primitives (the sign is dropped). Returns
// a static string; never NULL. Pure, so it renders identically on every target.
public U8 *StrError(I64 err);

// Write "<msg>: <StrError(err)>\n" to stderr, like C's `perror`. Takes `err`
// explicitly — pass either the negative return value you got back
// (`Perror("open", fd)`) or the global (`Perror("open", errno)`); C's no-argument
// form is `Perror(msg, errno)`.
public U0 Perror(U8 *msg, I64 err);

#endif
