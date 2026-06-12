// stdlib_div.hc — use stdlib Div() which returns (I64,I64)
#include <stdio.hh>
#include <stdlib.hh>
q, r := Div(23, 7);
"%d rem %d\n", q, r;
q2, r2 := Div(-17, 5);
"%d rem %d\n", q2, r2;
