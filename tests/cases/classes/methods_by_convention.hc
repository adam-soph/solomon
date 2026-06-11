// methods_by_convention.hc — methods-by-convention: functions taking Self *
class Counter { I64 count; };
U0 Inc(Counter *self) { self->count++; }
U0 Add(Counter *self, I64 n) { self->count = self->count + n; }
I64 Get(Counter *self) { return self->count; }
Counter c; c.count = 0;
Inc(&c); Inc(&c); Add(&c, 10);
"%d\n", Get(&c);
