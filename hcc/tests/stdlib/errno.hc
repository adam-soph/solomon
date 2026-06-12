
#include <errno.hh>
#include <stdio.hh>
#include <stdlib.hh>
U0 Main() {
  "%s\n", StrError(0);
  "%s\n", StrError(ENOENT);
  "%s\n", StrError(-ENOENT);    // a negative -errno is accepted too
  "%s\n", StrError(EINVAL);
  "%s\n", StrError(ENAMETOOLONG);
  "%s\n", StrError(ECONNREFUSED);
  "%s\n", StrError(99999);      // unknown -> generic
}
Main;
