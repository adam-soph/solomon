// ptr_to_other_class.hc — class containing a pointer to another class

#include <stdio.hh>
class Node { I64 val; Node *next; };
class Wrapper { Node *ptr; };
Node n; n.val = 42; n.next = NULL;
Wrapper w; w.ptr = &n;
"%d\n", w.ptr->val;
