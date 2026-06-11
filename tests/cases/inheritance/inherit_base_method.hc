// inherit_base_method.hc — method that takes Base * and calls another method
class Obj { I64 val; };
class Wrapper : Obj { I64 mult; };
I64 Compute(Obj *o) { return o->val * 2; }
Wrapper w; w.val = 7; w.mult = 3;
"%d\n", Compute((Obj *)&w);
