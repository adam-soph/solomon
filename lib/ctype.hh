#ifndef _CTYPE_HH
#define _CTYPE_HH
// ctype.hh — interface for ASCII character classification and case conversion in the
// "C" locale, the `<ctype.h>` family. Implementation in `ctype.hc`.
//
// Each `Is*` returns 0 or 1, deliberately not libc's unspecified nonzero. A fixed
// 0/1 keeps the interpreter and the backends in agreement. A byte outside every range
// classifies as false; this includes high bytes and negative or EOF-like values.
// Include with `#include <ctype.hh>`.

public I64 ToUpper(I64 c);
public I64 ToLower(I64 c);

public I64 IsDigit(I64 c);
public I64 IsUpper(I64 c);
public I64 IsLower(I64 c);
public I64 IsAlpha(I64 c);
public I64 IsAlNum(I64 c);
public I64 IsXDigit(I64 c);
public I64 IsSpace(I64 c);
public I64 IsBlank(I64 c);
public I64 IsCntrl(I64 c);
public I64 IsPrint(I64 c);
public I64 IsGraph(I64 c);
public I64 IsPunct(I64 c);

#endif
