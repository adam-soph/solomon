// modify_via_ptr_arg.hc — function modifies class fields through pointer arg
class Rect { I64 w; I64 h; };
U0 Scale(Rect *r, I64 factor) { r->w = r->w * factor; r->h = r->h * factor; }
Rect r; r.w = 4; r.h = 3;
Scale(&r, 2);
"%d %d\n", r.w, r.h;
