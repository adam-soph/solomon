// Global initializer: class with a brace initializer at global scope.
class Pt { I64 x; I64 y; };
Pt g_pt = {.x = 11, .y = 22};

"%d %d\n", g_pt.x, g_pt.y;
