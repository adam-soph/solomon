// sum_generic.hc — generic Sum over an array
#include <stdio.hh>
#include <stdlib.hh>
I64 SumI64(I64 *arr, I64 n) { I64 s = 0, i; for (i=0;i<n;i++) s+=arr[i]; return s; }
F64 SumF64(F64 *arr, I64 n) { F64 s = 0.0; I64 i; for (i=0;i<n;i++) s+=arr[i]; return s; }

I64 ia[5];
ia[0]=1; ia[1]=2; ia[2]=3; ia[3]=4; ia[4]=5;
"%d\n", SumI64(ia, 5);

// Heap-allocated so Sort can use it too
I64 *ha = MAlloc(4 * sizeof(I64));
ha[0]=10; ha[1]=20; ha[2]=30; ha[3]=40;
"%d\n", SumI64(ha, 4);
Free(ha);
