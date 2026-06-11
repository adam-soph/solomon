// Build a small linked list using heap nodes; print values; free nodes.
class Node {
  I64 val;
  Node *next;
};

Node *head = NULL;

U0 Push(I64 v) {
  Node *n = MAlloc(sizeof(Node));
  n->val = v;
  n->next = head;
  head = n;
}

Push(3);
Push(2);
Push(1);

Node *p = head;
while (p != NULL) {
  "%d ", p->val;
  p = p->next;
}
"\n";

// Free
p = head;
while (p != NULL) {
  Node *tmp = p->next;
  Free(p);
  p = tmp;
}
