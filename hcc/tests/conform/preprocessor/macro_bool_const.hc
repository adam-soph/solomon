// macro_bool_const.hc — TRUE/FALSE/NULL defined as object macros

#include <stdio.hh>
#define MY_TRUE 1
#define MY_FALSE 0
I64 flag = MY_TRUE;
if (flag) "yes\n";
flag = MY_FALSE;
if (!flag) "no\n";
"%d %d\n", MY_TRUE, MY_FALSE;
