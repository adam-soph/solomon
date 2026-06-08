#ifndef _ERRNO_HC
#define _ERRNO_HC
// errno.hc — C `<errno.h>`: named error codes, plus `StrError`/`Perror` to render
// them. The stdlib's OS primitives (`Open`/`Read`/`Write`/`Close`/`LSeek`/`Socket`/
// `Connect`/`Mkdir`/`Remove`/`Rename`/`Chdir`/`Getcwd`/…) report failure Unix-style:
// a non-negative result on success, a negative `-errno` on error. These constants and
// helpers name and describe that `-errno`. Include with `#include <errno.hc>`.
//
// The values are **Linux-canonical** and portable: the freestanding Linux targets
// return them directly, and the Darwin backend + interpreter normalise the host's
// native errno to these numbers, so `if (ret == -ENOENT)` behaves the same on every
// target. (Windows file ops currently surface a plain `-1`, so `StrError(-1)` is the
// generic "Unknown error" there until per-error mapping lands.)

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

#include <string.hc>   // StrLen (Perror)
#include <unistd.hc>   // StdWrite + STDERR (Perror)

// --- rendering ---------------------------------------------------------------

// The message for an errno, like C's `strerror`. Accepts either the positive code
// or the negative `-errno` returned by the primitives (the sign is dropped). Returns
// a static string; never NULL. Pure, so it renders identically on every target.
public U8 *StrError(I64 err)
{
  if (err < 0) err = -err;
  switch (err) {
    case 0:            return "Success";
    case EPERM:        return "Operation not permitted";
    case ENOENT:       return "No such file or directory";
    case EINTR:        return "Interrupted system call";
    case EIO:          return "Input/output error";
    case EBADF:        return "Bad file descriptor";
    case EAGAIN:       return "Resource temporarily unavailable";
    case ENOMEM:       return "Cannot allocate memory";
    case EACCES:       return "Permission denied";
    case EFAULT:       return "Bad address";
    case EBUSY:        return "Device or resource busy";
    case EEXIST:       return "File exists";
    case EXDEV:        return "Invalid cross-device link";
    case ENODEV:       return "No such device";
    case ENOTDIR:      return "Not a directory";
    case EISDIR:       return "Is a directory";
    case EINVAL:       return "Invalid argument";
    case ENFILE:       return "Too many open files in system";
    case EMFILE:       return "Too many open files";
    case EFBIG:        return "File too large";
    case ENOSPC:       return "No space left on device";
    case ESPIPE:       return "Illegal seek";
    case EROFS:        return "Read-only file system";
    case EPIPE:        return "Broken pipe";
    case ERANGE:       return "Numerical result out of range";
    case ENAMETOOLONG: return "File name too long";
    case ENOTEMPTY:    return "Directory not empty";
    case ELOOP:        return "Too many levels of symbolic links";
    case EADDRINUSE:   return "Address already in use";
    case ENETUNREACH:  return "Network is unreachable";
    case ECONNRESET:   return "Connection reset by peer";
    case ETIMEDOUT:    return "Connection timed out";
    case ECONNREFUSED: return "Connection refused";
    case EHOSTUNREACH: return "No route to host";
    case EINPROGRESS:  return "Operation now in progress";
    case ESRCH:        return "No such process";
    case ENXIO:        return "No such device or address";
    case E2BIG:        return "Argument list too long";
    case ENOEXEC:      return "Exec format error";
    case ECHILD:       return "No child processes";
    case ETXTBSY:      return "Text file busy";
    case EMLINK:       return "Too many links";
    case EDOM:         return "Numerical argument out of domain";
    case EDEADLK:      return "Resource deadlock avoided";
    case ENOSYS:       return "Function not implemented";
    case ENOTSOCK:     return "Socket operation on non-socket";
    case EDESTADDRREQ: return "Destination address required";
    case EMSGSIZE:     return "Message too long";
    case EPROTOTYPE:   return "Protocol wrong type for socket";
    case ENOPROTOOPT:  return "Protocol not available";
    case EPROTONOSUPPORT: return "Protocol not supported";
    case EOPNOTSUPP:   return "Operation not supported";
    case EAFNOSUPPORT: return "Address family not supported by protocol";
    case EADDRNOTAVAIL: return "Cannot assign requested address";
    case ENETDOWN:     return "Network is down";
    case ECONNABORTED: return "Software caused connection abort";
    case ENOBUFS:      return "No buffer space available";
    case EISCONN:      return "Transport endpoint is already connected";
    case ENOTCONN:     return "Transport endpoint is not connected";
    case EHOSTDOWN:    return "Host is down";
    case EALREADY:     return "Operation already in progress";
    case ECANCELED:    return "Operation canceled";
    default:           return "Unknown error";
  }
}

// Write "<msg>: <StrError(err)>\n" to stderr, like C's `perror`. Takes `err`
// explicitly (this stdlib has no global `errno`; the primitives return `-errno`), so
// pass the value you got back, e.g. `Perror("open", fd)`.
public U0 Perror(U8 *msg, I64 err)
{
  U8 *e = StrError(err);
  StdWrite(STDERR, msg, StrLen(msg));
  StdWrite(STDERR, ": ", 2);
  StdWrite(STDERR, e, StrLen(e));
  StdWrite(STDERR, "\n", 1);
}

#endif
