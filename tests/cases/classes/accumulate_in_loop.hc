// accumulate_in_loop.hc — accumulate into class fields in a loop
class Acc { I64 total; I64 count; };
Acc a; a.total = 0; a.count = 0;
I64 i;
for (i = 1; i <= 5; i++) { a.total = a.total + i; a.count++; }
"%d %d\n", a.total, a.count;
