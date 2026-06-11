// Initializer list element that is a function call result.
I64 Square(I64 n) { return n * n; }
I64 a[3] = {Square(2), Square(3), Square(4)};
"%d %d %d\n", a[0], a[1], a[2];
