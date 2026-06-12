// array_of_ptrs.hc — array of pointers to class instances

#include <stdio.hh>
class Node { I64 v; };
Node a; a.v = 10;
Node b; b.v = 20;
Node c; c.v = 30;
Node *arr[3];
arr[0] = &a; arr[1] = &b; arr[2] = &c;
I64 i;
for (i = 0; i < 3; i++) "%d\n", arr[i]->v;
