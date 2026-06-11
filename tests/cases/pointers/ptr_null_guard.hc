// Short-circuit null guard: p && p->x.
class Node {
  I64 val;
  Node *next;
};

Node a;
a.val = 42;
a.next = NULL;
Node *p = &a;
Node *q = NULL;

"%d\n", p && p->val;
"%d\n", q && q->val;
