// global_class_fields.hc — global class instance, fields read/written in functions
class Config { I64 width; I64 height; };
Config g_cfg;
U0 SetSize(I64 w, I64 h) { g_cfg.width = w; g_cfg.height = h; }
I64 Area() { return g_cfg.width * g_cfg.height; }
SetSize(80, 24);
"%d\n", Area();
