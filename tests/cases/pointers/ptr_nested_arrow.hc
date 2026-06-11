// Pointer-chased member: p->next->val.
class Node { I64 val; Node *next; };

Node a; a.val = 1; a.next = NULL;
Node b; b.val = 2; b.next = &a;
Node *p = &b;
"%d %d\n", p->val, p->next->val;
