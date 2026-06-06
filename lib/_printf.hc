#ifndef _PRINTF_HC
#define _PRINTF_HC
// _printf.hc — the private core of the printf family (`Print`/`StrPrint`/
// `CatPrint`/`MStrPrint`, declared in `<fmt.hc>`). `_VFmt` walks the format string,
// pulls the variadic slots, and renders each conversion into a sink (a fd via
// `StdWrite`, or a buffer). Integers and strings are laid out here, mirroring
// `crate::fmt::{parse, render_int, render_str}` byte-for-byte; floats delegate to
// `_FmtFloat`. The interpreter renders the printf family via `crate::fmt` (its
// independent conformance oracle) and never runs this, so the two cross-check.
//
// Private (the `_`-prefixed filename): user code prints via `Print` / `"%f", …`,
// never by calling these directly.

#include <stdio.hc>         // StdWrite (the fd sink) — NOT <io.hc>, to avoid compiling
                            // its file helpers (gated on unsupported targets) into every
                            // printing program (there is no dead-code elimination)
#include <mem.hc>           // ReAlloc (MStrPrint growth)
#include <cstr.hc>          // StrLen
#include <_fltfmt.hc>  // _FmtFloat + the _FLT_* flag bits

// Width/precision caps matching `crate::fmt::{MAX_WIDTH, MAX_PRECISION}`.
#define _PF_MAX_WIDTH 1024
#define _PF_MAX_PREC  512

// A formatted-output sink: a fd (when `dst` is NULL) or a buffer at `dst`, with `len`
// the bytes emitted so far (and the write cursor). `grow` ⇒ `ReAlloc` `dst` when a
// write would overflow `cap` (the `MStrPrint` growing buffer).
class _Pf { U8 *dst; I64 fd; I64 len; I64 cap; I64 grow; }

// Append `n` bytes to the sink.
U0 _PfPut(_Pf *p, U8 *buf, I64 n)
{
  if (n <= 0) return;
  if (!p->dst) { StdWrite(p->fd, buf, n); p->len += n; return; }
  if (p->grow && p->len + n + 1 > p->cap) {
    I64 ncap = p->cap * 2;
    if (ncap < p->len + n + 1) ncap = p->len + n + 1;
    p->dst = ReAlloc(p->dst, p->cap, ncap);
    p->cap = ncap;
  }
  I64 i;
  for (i = 0; i < n; i++) p->dst[p->len + i] = buf[i];
  p->len += n;
}

// Append `n` copies of byte `c` (field padding), batched so the fd sink isn't a
// syscall per byte.
U0 _PfFill(_Pf *p, I64 c, I64 n)
{
  U8 buf[64];
  I64 i;
  for (i = 0; i < 64; i++) buf[i] = c;
  while (n > 0) {
    I64 k = n;
    if (k > 64) k = 64;
    _PfPut(p, buf, k);
    n -= k;
  }
}

// `_IFld`/`_PfStrField` flag bits.
#define _PF_MINUS 1
#define _PF_ZERO  2

// The layout parameters of an integer field, bundled into one struct so `_PfIntField`
// stays within the x86-64 backend's 6-integer-parameter ABI limit. `sign` is one of
// "","-","+"," "; `alt` an alternate-form prefix ("","0x","0X"); `width`<0 ⇒ none;
// `prec`<0 ⇒ none; `flags` packs `_PF_MINUS`/`_PF_ZERO`.
class _IFld { U8 *sign; U8 *alt; I64 width; I64 prec; I64 flags; }

// Lay out an integer field (mirrors `crate::fmt::render_int`). `dig`/`ndig` are the
// magnitude digits, MSB-first (`ndig >= 1`, "0" for zero). With a precision the zero
// flag is ignored, and "0" at precision 0 yields no digits.
U0 _PfIntField(_Pf *p, U8 *dig, I64 ndig, _IFld *f)
{
  I64 minus = f->flags & _PF_MINUS, zero = f->flags & _PF_ZERO;
  I64 prec = f->prec;
  I64 slen = StrLen(f->sign), alen = StrLen(f->alt);
  // base = digits actually shown (before precision zero-fill); "0" with prec 0 ⇒ 0.
  I64 base = ndig;
  if (prec == 0 && ndig == 1 && dig[0] == '0') base = 0;
  I64 total = base;
  if (prec > total) total = prec; // precision pads with leading zeros to `prec`
  I64 lead = total - base;        // leading precision zeros
  I64 body = slen + alen + total;
  I64 w = 0;
  if (f->width > 0) w = f->width;
  I64 pad = w - body;

  if (pad > 0 && !minus && !(zero && prec < 0))
    _PfFill(p, ' ', pad);          // right-justified space pad goes first
  _PfPut(p, f->sign, slen);
  _PfPut(p, f->alt, alen);
  if (pad > 0 && !minus && zero && prec < 0)
    _PfFill(p, '0', pad);          // zero pad after the sign/prefix
  _PfFill(p, '0', lead);           // precision leading zeros
  if (base > 0) _PfPut(p, dig, ndig);
  if (pad > 0 && minus)
    _PfFill(p, ' ', pad);          // left-justified: trailing spaces
}

// Lay out a string/char field (mirrors `crate::fmt::render_str`): truncate `body` to
// `prec` bytes (prec<0 ⇒ none), then pad to `width` (left-justified with `minus`).
U0 _PfStrField(_Pf *p, U8 *body, I64 blen, I64 width, I64 prec, I64 minus)
{
  I64 len = blen;
  if (prec >= 0 && prec < len) len = prec;
  I64 w = 0;
  if (width > 0) w = width;
  I64 pad = w - len;
  if (pad > 0 && !minus) _PfFill(p, ' ', pad);
  _PfPut(p, body, len);
  if (pad > 0 && minus) _PfFill(p, ' ', pad);
}

// The magnitude of `n` (the I64 absolute value, wrapping so I64.MIN works) as an
// unsigned value.
U64 _PfMag(I64 n)
{
  if (n < 0) return -(U64)n;
  return n;
}

// Write the digits of `u` in `base` (MSB-first) into `dig`, returning the count.
// `upper` selects A–F vs a–f for hex.
I64 _PfDigits(U64 u, I64 base, I64 upper, U8 *dig)
{
  U8 tmp[24];
  I64 nd = 0;
  if (u == 0) tmp[nd++] = '0';
  else
    while (u) {
      I64 d = u % base;
      if (d < 10) tmp[nd++] = '0' + d;
      else tmp[nd++] = (upper ? 'A' : 'a') + d - 10;
      u /= base;
    }
  I64 i;
  for (i = 0; i < nd; i++) dig[i] = tmp[nd - 1 - i];
  return nd;
}

// Render `fmt` with the variadic slots `vargv[0..vargc)` into the sink `p`. Each slot
// is a raw 8-byte value: an I64 for integer/char conversions, the bit pattern of an
// F64 for `%f`/`%e`/`%g`, a `U8 *` for `%s`. `*` width/precision consume a slot.
U0 _VFmt(_Pf *p, U8 *fmt, I64 *vargv, I64 vargc)
{
  I64 ai = 0, i = 0;
  while (fmt[i]) {
    if (fmt[i] != '%') {
      I64 run = i;
      while (fmt[i] && fmt[i] != '%') i++;
      _PfPut(p, fmt + run, i - run);
      continue;
    }
    i++; // past '%'
    // ---- parse the spec (mirror crate::fmt::parse) ----
    I64 minus = 0, plus = 0, space = 0, zero = 0, hash = 0;
    I64 loop = 1;
    while (loop) {
      if (fmt[i] == '-') minus = 1;
      else if (fmt[i] == '+') plus = 1;
      else if (fmt[i] == ' ') space = 1;
      else if (fmt[i] == '0') zero = 1;
      else if (fmt[i] == '#') hash = 1;
      else loop = 0;
      if (loop) i++;
    }
    I64 width = -1;
    if (fmt[i] == '*') {
      i++;
      I64 w = 0;
      if (ai < vargc) w = vargv[ai++];
      if (w < 0) { minus = 1; width = -w; } else width = w;
    } else {
      I64 have = 0, w = 0;
      while (fmt[i] >= '0' && fmt[i] <= '9') { w = w * 10 + (fmt[i] - '0'); i++; have = 1; }
      if (have) width = w;
    }
    I64 prec = -1;
    if (fmt[i] == '.') {
      i++;
      if (fmt[i] == '*') {
        i++;
        I64 pv = 0;
        if (ai < vargc) pv = vargv[ai++];
        if (pv >= 0) prec = pv; // negative ⇒ omitted
      } else {
        I64 pp = 0;
        while (fmt[i] >= '0' && fmt[i] <= '9') { pp = pp * 10 + (fmt[i] - '0'); i++; }
        prec = pp;
      }
    }
    while (fmt[i] == 'l' || fmt[i] == 'h' || fmt[i] == 'L' || fmt[i] == 'z'
           || fmt[i] == 'j' || fmt[i] == 't') i++; // length modifiers: discard
    I64 conv = fmt[i];
    if (conv) i++;
    if (width > _PF_MAX_WIDTH) width = _PF_MAX_WIDTH;
    if (prec > _PF_MAX_PREC) prec = _PF_MAX_PREC;

    if (conv == '%') { _PfFill(p, '%', 1); continue; }

    // ---- dispatch ----
    // Shared integer-field layout params (sign/alt overridden per conversion).
    _IFld fld;
    fld.sign = "";
    fld.alt = "";
    fld.width = width;
    fld.prec = prec;
    fld.flags = minus | (zero << 1);
    if (conv == 'd' || conv == 'i') {
      I64 n = 0;
      if (ai < vargc) n = vargv[ai++];
      if (n < 0) fld.sign = "-";
      else if (plus) fld.sign = "+";
      else if (space) fld.sign = " ";
      U8 dig[24];
      I64 nd = _PfDigits(_PfMag(n), 10, 0, dig);
      _PfIntField(p, dig, nd, &fld);
    } else if (conv == 'u') {
      U64 u = 0;
      if (ai < vargc) u = vargv[ai++];
      U8 dig[24];
      I64 nd = _PfDigits(u, 10, 0, dig);
      _PfIntField(p, dig, nd, &fld);
    } else if (conv == 'x' || conv == 'X') {
      U64 u = 0;
      if (ai < vargc) u = vargv[ai++];
      U8 dig[24];
      I64 nd = _PfDigits(u, 16, conv == 'X', dig);
      if (hash && u != 0) fld.alt = (conv == 'x') ? "0x" : "0X";
      _PfIntField(p, dig, nd, &fld);
    } else if (conv == 'o') {
      U64 u = 0;
      if (ai < vargc) u = vargv[ai++];
      U8 dig[26];
      I64 nd = _PfDigits(u, 8, 0, dig);
      if (hash) {
        if (dig[0] != '0') { // prepend a leading 0 (shift right by one)
          I64 k;
          for (k = nd; k > 0; k--) dig[k] = dig[k - 1];
          dig[0] = '0';
          nd++;
        }
        if (fld.prec == 0 && nd == 1 && dig[0] == '0') fld.prec = 1; // keep the 0
      }
      _PfIntField(p, dig, nd, &fld);
    } else if (conv == 'c') {
      U8 ch = 0;
      if (ai < vargc) ch = vargv[ai++];
      _PfStrField(p, &ch, 1, width, -1, minus); // %c ignores precision
    } else if (conv == 's') {
      U8 *str = "(null)";
      if (ai < vargc) { str = *(U8 **)(&vargv[ai]); ai++; }
      if (!str) str = "(null)";
      _PfStrField(p, str, StrLen(str), width, prec, minus);
    } else if (conv == 'f' || conv == 'e' || conv == 'E' || conv == 'g' || conv == 'G') {
      F64 v = 0.0;
      if (ai < vargc) { v = *(F64 *)(&vargv[ai]); ai++; }
      I64 flags = 0;
      if (minus) flags |= _FLT_MINUS;
      if (zero) flags |= _FLT_ZERO;
      if (plus) flags |= _FLT_PLUS;
      if (space) flags |= _FLT_SPACE;
      if (hash) flags |= _FLT_HASH;
      I64 fp = (prec >= 0) ? prec : 6; // float default precision is 6
      U8 fbuf[2400];
      I64 flen = _FmtFloat(fbuf, v, conv, flags, width < 0 ? 0 : width, fp);
      _PfPut(p, fbuf, flen);
    }
    // an unknown conversion is silently dropped (matches a no-op spec)
  }
}

#endif
