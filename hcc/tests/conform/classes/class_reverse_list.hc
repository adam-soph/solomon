// class_reverse_list.hc — reverse a pool-allocated linked list

#include <stdio.hh>
#include <stdlib.hh>
class Node { I64 v; Node *nx; };
Node pool[5];
I64 used = 0;
Node *Alloc2(I64 v) {
  Node *n = &pool[used]; used++;
  n->v = v; n->nx = NULL; return n;
}
Node *head = NULL;
U0 Push2(I64 v) { Node *n = Alloc2(v); n->nx = head; head = n; }
Push2(1); Push2(2); Push2(3);
// Reverse
Node *prev = NULL; Node *cur = head;
while (cur != NULL) {
  Node *nx = cur->nx;
  cur->nx = prev;
  prev = cur;
  cur = nx;
}
head = prev;
Node *it = head;
while (it != NULL) { "%d\n", it->v; it = it->nx; }
