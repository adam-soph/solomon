// Use ** to insert at front of a list via double-pointer.

#include <stdio.hh>
#include <stdlib.hh>
class Node {
  I64 val;
  Node *next;
};

#define POOL 8
Node pool[POOL];
I64 used = 0;

Node *Alloc(I64 v) {
  Node *n = &pool[used]; used++;
  n->val = v; n->next = NULL;
  return n;
}

U0 Prepend(Node **head, I64 v) {
  Node *n = Alloc(v);
  n->next = *head;
  *head = n;
}

Node *head = NULL;
Prepend(&head, 30);
Prepend(&head, 20);
Prepend(&head, 10);

Node *p = head;
while (p != NULL) { "%d ", p->val; p = p->next; }
"\n";
