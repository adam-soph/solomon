// A #define-sized global pool array used as an allocator.
#define POOL_SIZE 8

class Node { I64 val; Node *next; };

Node g_pool[POOL_SIZE];
I64 g_used;

Node *Alloc(I64 v) {
  Node *n = &g_pool[g_used]; g_used++;
  n->val = v; n->next = NULL;
  return n;
}

Node *a = Alloc(1);
Node *b = Alloc(2);
Node *c = Alloc(3);
a->next = b; b->next = c;
Node *p = a;
while (p != NULL) { "%d ", p->val; p = p->next; }
"\n";
