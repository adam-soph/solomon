// nested_ptr_chain.hc — class with a nested class field and a pointer
class Inner { I64 val; };
class Outer { Inner inner; Inner *ptr; };
Inner ix; ix.val = 7;
Outer o; o.inner.val = 3; o.ptr = &ix;
"%d %d\n", o.inner.val, o.ptr->val;
