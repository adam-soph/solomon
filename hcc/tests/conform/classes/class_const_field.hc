// class_const_field.hc — initialize a class field to a compile-time constant

#include <stdio.hh>
#define MAGIC 0xDEAD
class Token { I64 tag; I64 val; };
Token t; t.tag = MAGIC; t.val = 42;
"%d %d\n", t.tag, t.val;
