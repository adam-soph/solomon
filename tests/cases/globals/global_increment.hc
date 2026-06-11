// Global counter incremented across multiple function calls.
I64 g_count;

U0 Bump() { g_count++; }

Bump(); Bump(); Bump();
"%d\n", g_count;
