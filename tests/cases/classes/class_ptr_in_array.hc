// class_ptr_in_array.hc — array of class instances, pointer into interior
class Val { I64 n; };
Val arr[4];
arr[0].n = 10; arr[1].n = 20; arr[2].n = 30; arr[3].n = 40;
Val *mid = &arr[2];
"%d\n", mid->n;
mid->n = 99;
"%d\n", arr[2].n;
