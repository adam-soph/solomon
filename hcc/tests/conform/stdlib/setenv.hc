// SetEnv/UnsetEnv/PutEnv override the environment for Getenv.
#include <errno.hh>
#include <stdio.hh>
#include <stdlib.hh>
"%d\n", Getenv("HCC_SETENV_T") == NULL;     // not set yet
SetEnv("HCC_SETENV_T", "one");
"%s\n", Getenv("HCC_SETENV_T");
SetEnv("HCC_SETENV_T", "two");              // overwrite in place
"%s\n", Getenv("HCC_SETENV_T");
UnsetEnv("HCC_SETENV_T");
"%d\n", Getenv("HCC_SETENV_T") == NULL;     // tombstoned
SetEnv("HCC_SETENV_T", "three");            // resettable after unset
"%s\n", Getenv("HCC_SETENV_T");
PutEnv("HCC_PUTENV_T=four");
"%s\n", Getenv("HCC_PUTENV_T");
PutEnv("HCC_PUTENV_T");                     // no '=': unsets
"%d\n", Getenv("HCC_PUTENV_T") == NULL;
"%d %d\n", SetEnv("", "x"), SetEnv("A=B", "x");  // invalid names -> -EINVAL
