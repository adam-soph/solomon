// debug_trace_macro.hc — debug trace macro that compiles out when off
#define TRACE_ON 1

#if TRACE_ON
#define TRACE(msg) "TRACE: %s\n", msg
#else
#define TRACE(msg)
#endif

TRACE("init");
I64 x = 42;
TRACE("computed");
"%d\n", x;
