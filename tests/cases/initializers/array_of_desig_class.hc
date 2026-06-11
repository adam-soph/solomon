// Array of classes each initialized with designated inits.
class Pt { I64 x; I64 y; };
Pt arr[3] = {{.x=1,.y=2},{.y=4,.x=3},{.x=5}};
I64 i;
for (i = 0; i < 3; i++)
  "%d %d\n", arr[i].x, arr[i].y;
