// generic_node.hc — generic linked node (singly linked list)

#include <stdio.hh>
class Node<type T> { T val; Node<T> *next; };

Node<I64> *NewNode(I64 v) {
  Node<I64> *n = MAlloc(sizeof(Node<I64>));
  n->val = v; n->next = NULL;
  return n;
}

Node<I64> *head = NULL;
I64 i;
for (i = 5; i >= 1; i--) {
  Node<I64> *n = NewNode(i);
  n->next = head;
  head = n;
}
Node<I64> *cur = head;
while (cur != NULL) {
  "%d ", cur->val;
  cur = cur->next;
}
"\n";
// free
cur = head;
while (cur != NULL) {
  Node<I64> *nxt = cur->next;
  Free(cur);
  cur = nxt;
}
