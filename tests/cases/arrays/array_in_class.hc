// Class containing a fixed array; read and write the array elements.
class Buf {
  I64 data[4];
  I64 n;
};

Buf b;
b.n = 4;
b.data[0] = 10;
b.data[1] = 20;
b.data[2] = 30;
b.data[3] = 40;
I64 i;
for (i = 0; i < b.n; i++) "%d ", b.data[i];
"\n";
