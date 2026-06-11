// class_in_function.hc — class declared and used inside a function
class Pair { I64 first; I64 second; };
I64 Sum(Pair p) { return p.first + p.second; }
I64 Diff(Pair p) { return p.first - p.second; }
Pair q; q.first = 15; q.second = 7;
"%d %d\n", Sum(q), Diff(q);
