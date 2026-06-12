// generic_stack.hc — a simple generic stack using Vec
#include <stdio.hh>
#include <stdlib.hh>
#include <vec.hh>
U0 StackPush<type T>(Vec<T> *s, T x) { VecPush(s, x); }
T StackPop<type T>(Vec<T> *s)        { return VecPop(s); }
I64 StackEmpty<type T>(Vec<T> *s)    { return VecLen(s) == 0; }

Vec<I64> s;
VecInit(&s);
StackPush(&s, 10);
StackPush(&s, 20);
StackPush(&s, 30);
while (!StackEmpty(&s))
  "%d\n", StackPop(&s);
VecFree(&s);
