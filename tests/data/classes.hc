// classes.hc — class and union definitions, pointers, member access.
class Point
{
  I64 x;
  I64 y;
};

// A self-referential class plus a base reference (registered while parsing).
class Node
{
  I64 value;
  Node *next;
};

union Reg
{
  U64 r;
  U32 e[2];
};

U0 Main()
{
  Point p;
  p.x = 3;
  p.y = 4;

  Point *pp = &p;
  pp->x = pp->x + 1;

  Node head;
  head.value = 10;
  head.next = NULL;
}
