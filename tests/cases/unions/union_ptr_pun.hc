// union_ptr_pun.hc — union of a pointer and I64, store int read as ptr and vice versa
union PtrInt { I64 i; I64 *p; };
I64 val = 42;
PtrInt x; x.p = &val;
// Reading as I64 gives the address (non-deterministic), but we can write an int
// and read it as int safely:
PtrInt y; y.i = 123;
"%d\n", y.i;
