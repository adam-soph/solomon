// linked_pool.hc — pool-allocated linked list, sum then walk backwards via prev
class Node { I64 val; Node *prev; };
Node pool[5];
I64 i;
for (i = 0; i < 5; i++) { pool[i].val = (i + 1) * 10; pool[i].prev = (i > 0) ? &pool[i-1] : NULL; }
Node *cur = &pool[4];
while (cur != NULL) { "%d\n", cur->val; cur = cur->prev; }
