#ifndef _CTYPE_HC
#define _CTYPE_HC
// ctype.hc — implementation of the `<ctype.h>` family (interface in `ctype.hh`).

#include <ctype.hh>

public I64 ToUpper(I64 c) { if (c >= 'a' && c <= 'z') return c - 32; return c; }
public I64 ToLower(I64 c) { if (c >= 'A' && c <= 'Z') return c + 32; return c; }

public I64 IsDigit(I64 c)  { return c >= '0' && c <= '9'; }
public I64 IsUpper(I64 c)  { return c >= 'A' && c <= 'Z'; }
public I64 IsLower(I64 c)  { return c >= 'a' && c <= 'z'; }
public I64 IsAlpha(I64 c)  { return IsUpper(c) || IsLower(c); }
public I64 IsAlNum(I64 c)  { return IsAlpha(c) || IsDigit(c); }
public I64 IsXDigit(I64 c) { return IsDigit(c) || (c >= 'A' && c <= 'F') || (c >= 'a' && c <= 'f'); }
public I64 IsSpace(I64 c)  { return (c >= 0x09 && c <= 0x0d) || c == ' '; }  // \t\n\v\f\r space
public I64 IsBlank(I64 c)  { return c == '\t' || c == ' '; }
public I64 IsCntrl(I64 c)  { return (c >= 0 && c <= 0x1f) || c == 0x7f; }
public I64 IsPrint(I64 c)  { return c >= ' ' && c <= 0x7e; }
public I64 IsGraph(I64 c)  { return c >= 0x21 && c <= 0x7e; }
public I64 IsPunct(I64 c)  { return IsGraph(c) && !IsAlNum(c); }

#endif
