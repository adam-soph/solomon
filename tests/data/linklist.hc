// linklist.hc — an array-backed singly linked list with sorted insertion.
// Exercises classes, self-referential pointers, double pointers (`Node **`),
// pointer-into-array allocation, short-circuit guards against null deref, and a
// classic iterative algorithm.

#define POOL_SIZE 16

class Node {
  I64 value;
  Node *next;
};

// A fixed pool we hand out nodes from (this subset has no heap).
Node g_pool[POOL_SIZE];
I64 g_used;

Node *Alloc(I64 value) {
  if (g_used >= POOL_SIZE)
    return NULL;
  Node *n = &g_pool[g_used];
  g_used++;
  n->value = value;
  n->next = NULL;
  return n;
}

// Insert `value` into the ascending list addressed by `head`.
U0 SortedInsert(Node **head, I64 value) {
  Node *node = Alloc(value);
  if (node == NULL)
    return;
  // Empty list, or new minimum: link at the front.
  if (*head == NULL || (*head)->value >= value) {
    node->next = *head;
    *head = node;
    return;
  }
  Node *cur = *head;
  while (cur->next != NULL && cur->next->value < value)
    cur = cur->next;
  node->next = cur->next;
  cur->next = node;
}

U0 PrintList(Node *head) {
  Node *p = head;
  while (p != NULL) {
    "%d ", p->value;
    p = p->next;
  }
  "\n";
}

I64 ListLength(Node *head) {
  I64 n = 0;
  Node *p = head;
  while (p != NULL) {
    n++;
    p = p->next;
  }
  return n;
}

I64 Gcd(I64 a, I64 b) {
  while (b != 0) {
    I64 t = b;
    b = a % b;
    a = t;
  }
  return a;
}

U0 Main() {
  g_used = 0;
  Node *head = NULL;

  I64 data[7];
  data[0] = 5;
  data[1] = 2;
  data[2] = 8;
  data[3] = 1;
  data[4] = 9;
  data[5] = 3;
  data[6] = 7;

  I64 i;
  for (i = 0; i < 7; i++)
    SortedInsert(&head, data[i]);

  "sorted: ";
  PrintList(head);
  "length=%d gcd(48,36)=%d\n", ListLength(head), Gcd(48, 36);
}

Main;
