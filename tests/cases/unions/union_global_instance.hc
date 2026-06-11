// union_global_instance.hc — global union instance written in a function
union G { I64 i; U64 u; };
G g_u;
U0 Set(I64 v) { g_u.i = v; }
Set(1234);
"%d\n", g_u.i;
