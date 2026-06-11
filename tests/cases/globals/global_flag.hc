// Global flag toggled by a function.
Bool g_active;

U0 Toggle() { g_active = !g_active; }

"%d\n", g_active;
Toggle();
"%d\n", g_active;
Toggle();
"%d\n", g_active;
