// Global U8* pointer to a string literal; print via %s.
#include <stdio.hc>

U8 *g_msg;

g_msg = "hello\n";
Print("%s", g_msg);
