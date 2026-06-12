// chain_of_nodes.hc — chain of Node structs on the stack via pointer links

#include <stdio.hh>
class Node { I64 v; Node *nx; };
Node n0; n0.v = 100; n0.nx = NULL;
Node n1; n1.v = 200; n1.nx = &n0;
Node n2; n2.v = 300; n2.nx = &n1;
Node *cur = &n2;
I64 sum = 0;
while (cur != NULL) { sum = sum + cur->v; cur = cur->nx; }
"%d\n", sum;
