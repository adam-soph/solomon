// linked_list_stack.hc — self-referential Node linked list, pool allocated
class Node { I64 val; Node *next; };
Node pool[8];
I64 used = 0;
Node *Alloc(I64 v) {
  Node *n = &pool[used]; used++;
  n->val = v; n->next = NULL;
  return n;
}
Node *head = NULL;
U0 Push(I64 v) {
  Node *n = Alloc(v);
  n->next = head;
  head = n;
}
Push(1); Push(2); Push(3);
Node *cur = head;
while (cur != NULL) { "%d\n", cur->val; cur = cur->next; }
