// macro_conditional_choice.hc — two code paths that print differently
#define DEBUG_MODE 0

U0 Log(U8 *msg) {
#if DEBUG_MODE
  "[DBG] %s\n", msg;
#else
  "%s\n", msg;
#endif
}
Log("start");
Log("finish");
