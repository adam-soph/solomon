// json.hc — a recursive-descent JSON parser written in HolyC. It parses a JSON
// document into a heap-allocated tree of tagged `JVal` nodes (objects, arrays,
// strings, integers, reals, true/false/null) and then queries it. Exercises
// mutual recursion, MAlloc/StrCpy/StrCmp, pointer-to-pointer arrays, F64
// arithmetic, and escape-aware string scanning — all conformant between the
// interpreter and the native backend.
//
// Real numbers parse into an F64 (`J_REAL`); the showcase reports them through
// integer-derived output rather than raw `%f`, because the two backends format
// `%f` differently (the interpreter trims, libc pads to six decimals) — the one
// documented divergence. Simplifications: number exponents (`1e3`) are not
// handled, and containers hold up to 32 elements.

#include <string.hc>

#define J_NULL 0
#define J_BOOL 1
#define J_NUM  2
#define J_STR  3
#define J_ARR  4
#define J_OBJ  5
#define J_REAL 6

class JVal {
  I64 kind;
  I64 num;      // J_NUM value, or J_BOOL (0/1)
  F64 fnum;     // J_REAL value
  U8 *str;      // J_STR text
  I64 count;    // J_ARR / J_OBJ element count
  JVal **items; // J_ARR / J_OBJ values
  U8 **keys;    // J_OBJ keys (parallel to items)
}

class Parser {
  U8 *src;
  I64 pos;
}

JVal *NewVal(I64 kind) {
  JVal *v = MAlloc(sizeof(JVal));
  v->kind = kind;
  v->num = 0;
  v->fnum = 0.0;
  v->str = NULL;
  v->count = 0;
  v->items = NULL;
  v->keys = NULL;
  return v;
}

U0 SkipWS(Parser *p) {
  U8 c;
  while (1) {
    c = p->src[p->pos];
    if (c == ' ' || c == '\n' || c == '\t' || c == '\r')
      p->pos++;
    else
      return;
  }
}

// Copy a "..."-delimited string (cursor on the opening quote) onto the heap,
// decoding the common JSON escapes. The decoded form is no longer than the
// source, so one generous buffer suffices.
U8 *ParseRawString(Parser *p) {
  p->pos++; // opening quote
  U8 *s = MAlloc(256);
  I64 j = 0;
  U8 c;
  while (p->src[p->pos] != '"' && p->src[p->pos] != 0) {
    c = p->src[p->pos];
    if (c == '\\') {
      p->pos++;
      U8 e = p->src[p->pos];
      if (e == 'n')
        c = '\n';
      else if (e == 't')
        c = '\t';
      else if (e == 'r')
        c = '\r';
      else
        c = e; // \" \\ \/ and any other: take the next char literally
    }
    s[j] = c;
    j++;
    p->pos++;
  }
  s[j] = 0;
  p->pos++; // closing quote
  return s;
}

// Match a keyword at the cursor; advance and return 1 on success, else 0.
I64 MatchKW(Parser *p, U8 *kw) {
  I64 i = 0;
  while (kw[i] != 0) {
    if (p->src[p->pos + i] != kw[i])
      return 0;
    i++;
  }
  p->pos += i;
  return 1;
}

JVal *ParseValue(Parser *p);

JVal *ParseNumber(Parser *p) {
  I64 sign = 1;
  if (p->src[p->pos] == '-') {
    sign = -1;
    p->pos++;
  }
  I64 n = 0;
  while (p->src[p->pos] >= '0' && p->src[p->pos] <= '9') {
    n = n * 10 + (p->src[p->pos] - '0');
    p->pos++;
  }
  if (p->src[p->pos] == '.') { // a real number — accumulate the fraction in F64
    p->pos++;
    F64 f = n;
    F64 scale = 0.1;
    while (p->src[p->pos] >= '0' && p->src[p->pos] <= '9') {
      f = f + (p->src[p->pos] - '0') * scale;
      scale = scale / 10.0;
      p->pos++;
    }
    JVal *r = NewVal(J_REAL);
    r->fnum = sign * f;
    return r;
  }
  JVal *v = NewVal(J_NUM);
  v->num = sign * n;
  return v;
}

JVal *ParseArray(Parser *p) {
  JVal *v = NewVal(J_ARR);
  v->items = MAlloc(32 * 8);
  p->pos++; // '['
  SkipWS(p);
  if (p->src[p->pos] == ']') {
    p->pos++;
    return v;
  }
  while (1) {
    v->items[v->count] = ParseValue(p);
    v->count++;
    SkipWS(p);
    if (p->src[p->pos] == ',') {
      p->pos++;
      continue;
    }
    break;
  }
  if (p->src[p->pos] == ']')
    p->pos++;
  return v;
}

JVal *ParseObject(Parser *p) {
  JVal *v = NewVal(J_OBJ);
  v->items = MAlloc(32 * 8);
  v->keys = MAlloc(32 * 8);
  p->pos++; // '{'
  SkipWS(p);
  if (p->src[p->pos] == '}') {
    p->pos++;
    return v;
  }
  while (1) {
    SkipWS(p);
    v->keys[v->count] = ParseRawString(p);
    SkipWS(p);
    if (p->src[p->pos] == ':')
      p->pos++;
    v->items[v->count] = ParseValue(p);
    v->count++;
    SkipWS(p);
    if (p->src[p->pos] == ',') {
      p->pos++;
      continue;
    }
    break;
  }
  if (p->src[p->pos] == '}')
    p->pos++;
  return v;
}

JVal *ParseValue(Parser *p) {
  SkipWS(p);
  U8 c = p->src[p->pos];
  if (c == '{')
    return ParseObject(p);
  if (c == '[')
    return ParseArray(p);
  if (c == '"')
    return ParseString(p);
  if (c == 't') {
    JVal *v = NewVal(J_BOOL);
    v->num = 1;
    MatchKW(p, "true");
    return v;
  }
  if (c == 'f') {
    JVal *v = NewVal(J_BOOL);
    MatchKW(p, "false");
    return v;
  }
  if (c == 'n') {
    JVal *v = NewVal(J_NULL);
    MatchKW(p, "null");
    return v;
  }
  return ParseNumber(p);
}

JVal *ParseString(Parser *p) {
  JVal *v = NewVal(J_STR);
  v->str = ParseRawString(p);
  return v;
}

// Look up a key in an object; return its value or NULL.
JVal *ObjGet(JVal *obj, U8 *key) {
  I64 i;
  for (i = 0; i < obj->count; i++)
    if (StrCmp(obj->keys[i], key) == 0)
      return obj->items[i];
  return NULL;
}

// --- pretty-printer: serialize a JVal tree back to compact JSON text ---

// Print a real with a fixed two decimals using integer math, so the output is
// byte-identical in both backends (raw %f formats differently — see the header).
U0 DumpReal(F64 f) {
  if (f < 0) {
    "-";
    f = -f;
  }
  I64 whole = f;
  I64 frac = (f - whole) * 100.0 + 0.5;
  if (frac >= 100) {
    whole++;
    frac = frac - 100;
  }
  "%d.", whole;
  if (frac < 10)
    "0";
  "%d", frac;
}

// Print a string as a JSON literal, re-escaping the characters we decode.
U0 DumpStr(U8 *s) {
  "\"";
  I64 i = 0;
  U8 c;
  while (s[i] != 0) {
    c = s[i];
    if (c == '"')
      "\\\"";
    else if (c == '\\')
      "\\\\";
    else if (c == '\n')
      "\\n";
    else if (c == '\t')
      "\\t";
    else
      "%c", c;
    i++;
  }
  "\"";
}

U0 Dump(JVal *v) {
  I64 i;
  switch [v->kind] { // the bracketed switch form, dispatching on the tag
    case J_NULL:
      "null";
      break;
    case J_BOOL:
      if (v->num)
        "true";
      else
        "false";
      break;
    case J_NUM:
      "%d", v->num;
      break;
    case J_REAL:
      DumpReal(v->fnum);
      break;
    case J_STR:
      DumpStr(v->str);
      break;
    case J_ARR:
      "[";
      for (i = 0; i < v->count; i++) {
        if (i > 0)
          ",";
        Dump(v->items[i]);
      }
      "]";
      break;
    case J_OBJ:
      "{";
      for (i = 0; i < v->count; i++) {
        if (i > 0)
          ",";
        DumpStr(v->keys[i]);
        ":";
        Dump(v->items[i]);
      }
      "}";
      break;
  }
}

U0 Main() {
  U8 *json = MAlloc(512);
  StrCpy(json, "{ \"name\": \"solomon\", \"version\": 2, \"pi\": 3.14, ");
  StrCat(json, "\"tags\": [\"holyc\", \"rust\", \"jit\"], ");
  StrCat(json, "\"path\": \"C:\\\\tmp\\\\j\", \"q\": \"say \\\"hi\\\"\", ");
  StrCat(json, "\"stable\": true, \"meta\": null, \"nested\": {\"depth\": 3} }");

  Parser p;
  p.src = json;
  p.pos = 0;
  JVal *root = ParseValue(&p);

  "kind=%d count=%d\n", root->kind, root->count;
  "name=%s version=%d\n", ObjGet(root, "name")->str, ObjGet(root, "version")->num;
  JVal *pi = ObjGet(root, "pi");
  "pi: kind=%d x100=%d\n", pi->kind, (I64)(pi->fnum * 100.0 + 0.5);
  JVal *tags = ObjGet(root, "tags");
  "tags=%d [%s, %s]\n", tags->count, tags->items[0]->str, tags->items[2]->str;
  "path=%s q=%s\n", ObjGet(root, "path")->str, ObjGet(root, "q")->str;
  "stable=%d meta=%d\n", ObjGet(root, "stable")->num, ObjGet(root, "meta")->kind;
  "nested.depth=%d\n", ObjGet(ObjGet(root, "nested"), "depth")->num;
  "json=";
  Dump(root);
  "\n";

  Free(json);
}

Main;
