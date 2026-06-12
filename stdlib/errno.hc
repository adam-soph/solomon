#ifndef _ERRNO_HC
#define _ERRNO_HC
// errno.hc — implementation (interface in errno.hh).

#include <errno.hh>
#include <string.hh>
#include <unistd.hh>

public I64 Errno() { return Fs->err; }
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
    case ENOTBLK:      return "Block device required";
    case ENOLCK:       return "No locks available";
    case ENOMSG:       return "No message of desired type";
    case EIDRM:        return "Identifier removed";
    case ENOSTR:       return "Device not a stream";
    case ENODATA:      return "No data available";
    case ETIME:        return "Timer expired";
    case ENOSR:        return "Out of streams resources";
    case ENOLINK:      return "Link has been severed";
    case EPROTO:       return "Protocol error";
    case EMULTIHOP:    return "Multihop attempted";
    case EBADMSG:      return "Bad message";
    case EOVERFLOW:    return "Value too large for defined data type";
    case EILSEQ:       return "Invalid or incomplete multibyte or wide character";
    case EUSERS:       return "Too many users";
    case ESOCKTNOSUPPORT: return "Socket type not supported";
    case EPFNOSUPPORT: return "Protocol family not supported";
    case ENETRESET:    return "Network dropped connection on reset";
    case ESHUTDOWN:    return "Cannot send after transport endpoint shutdown";
    case ETOOMANYREFS: return "Too many references: cannot splice";
    case ESTALE:       return "Stale file handle";
    case EDQUOT:       return "Disk quota exceeded";
    case EOWNERDEAD:   return "Owner died";
    case ENOTRECOVERABLE: return "State not recoverable";
    default:           return "Unknown error";
  }
}
public U0 Perror(U8 *msg, I64 err)
{
  U8 *e = StrError(err);
  StdWrite(STDERR, msg, StrLen(msg));
  StdWrite(STDERR, ": ", 2);
  StdWrite(STDERR, e, StrLen(e));
  StdWrite(STDERR, "\n", 1);
}

#endif
