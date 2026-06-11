// Array of classes initialized with brace lists.
class Pair { I64 a; I64 b; };
Pair arr[3] = {{1,2},{3,4},{5,6}};
I64 i;
for (i = 0; i < 3; i++)
  "%d %d\n", arr[i].a, arr[i].b;
