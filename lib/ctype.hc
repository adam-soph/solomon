#ifndef _CTYPE_HC
#define _CTYPE_HC
// ctype.hc — ASCII character classification and case conversion ("C" locale), the
// `<ctype.h>` family.
//
// Each `Is*` returns 0/1 (deliberately not libc's unspecified nonzero, which would
// diverge between the interpreter and the backends). A byte outside every range —
// including high bytes and negative/EOF-like values — classifies as false.
// Include with `#include <ctype.hc>`.

I64 ToUpper(I64 c) { if (c >= 'a' && c <= 'z') return c - 32; return c; }
I64 ToLower(I64 c) { if (c >= 'A' && c <= 'Z') return c + 32; return c; }

I64 IsDigit(I64 c)  { return c >= '0' && c <= '9'; }
I64 IsUpper(I64 c)  { return c >= 'A' && c <= 'Z'; }
I64 IsLower(I64 c)  { return c >= 'a' && c <= 'z'; }
I64 IsAlpha(I64 c)  { return IsUpper(c) || IsLower(c); }
I64 IsAlNum(I64 c)  { return IsAlpha(c) || IsDigit(c); }
I64 IsXDigit(I64 c) { return IsDigit(c) || (c >= 'A' && c <= 'F') || (c >= 'a' && c <= 'f'); }
I64 IsSpace(I64 c)  { return (c >= 0x09 && c <= 0x0d) || c == ' '; }  // \t\n\v\f\r space
I64 IsBlank(I64 c)  { return c == '\t' || c == ' '; }
I64 IsCntrl(I64 c)  { return (c >= 0 && c <= 0x1f) || c == 0x7f; }
I64 IsPrint(I64 c)  { return c >= ' ' && c <= 0x7e; }
I64 IsGraph(I64 c)  { return c >= 0x21 && c <= 0x7e; }
I64 IsPunct(I64 c)  { return IsGraph(c) && !IsAlNum(c); }

#endif
