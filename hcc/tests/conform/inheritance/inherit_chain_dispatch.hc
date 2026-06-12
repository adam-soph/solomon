// inherit_chain_dispatch.hc — chain 3 levels, dispatch by kind field

#include <stdio.hh>
class Node { I64 kind; };
class Leaf : Node { I64 leaf_val; };
class Inner : Node { I64 left_val; I64 right_val; };

I64 NodeVal(Node *n) {
  switch (n->kind) {
    case 0: return ((Leaf *)n)->leaf_val;
    case 1: return ((Inner *)n)->left_val + ((Inner *)n)->right_val;
  }
  return 0;
}

Leaf l; l.kind = 0; l.leaf_val = 7;
Inner in; in.kind = 1; in.left_val = 3; in.right_val = 4;
"%d\n", NodeVal((Node *)&l);
"%d\n", NodeVal((Node *)&in);
