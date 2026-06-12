// inherit_accumulate.hc — accumulate a value from a heterogeneous array

#include <stdio.hh>
class Item { I64 kind; };
class Scored : Item { I64 score; };
Scored s0; s0.kind = 1; s0.score = 10;
Scored s1; s1.kind = 1; s1.score = 20;
Scored s2; s2.kind = 1; s2.score = 30;
Item *arr[3]; arr[0] = (Item *)&s0; arr[1] = (Item *)&s1; arr[2] = (Item *)&s2;
I64 total = 0; I64 i;
for (i = 0; i < 3; i++) if (arr[i]->kind == 1) total = total + ((Scored *)arr[i])->score;
"%d\n", total;
