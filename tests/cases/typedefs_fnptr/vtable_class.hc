// Vtable pattern: class with multiple fn-ptr fields.
class Shape {
  F64 (*area)(F64, F64);
  F64 (*perimeter)(F64, F64);
};

F64 RectArea(F64 w, F64 h) { return w * h; }
F64 RectPerim(F64 w, F64 h) { return 2.0 * (w + h); }

Shape rect;
rect.area = &RectArea;
rect.perimeter = &RectPerim;

"%.1f\n", rect.area(3.0, 4.0);
"%.1f\n", rect.perimeter(3.0, 4.0);
