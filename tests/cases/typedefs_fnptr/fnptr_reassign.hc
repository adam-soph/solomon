// Function pointer reassigned to different functions.
I64 Add(I64 a, I64 b) { return a + b; }
I64 Sub(I64 a, I64 b) { return a - b; }
I64 Mul(I64 a, I64 b) { return a * b; }

I64 (*f)(I64, I64) = &Add;
"%d\n", f(5, 3);
f = &Sub; "%d\n", f(5, 3);
f = &Mul; "%d\n", f(5, 3);
