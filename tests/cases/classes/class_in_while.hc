// class_in_while.hc — iterate using a class to hold loop state
class State { I64 n; I64 sum; };
State s; s.n = 1; s.sum = 0;
while (s.n <= 10) { s.sum = s.sum + s.n; s.n++; }
"%d\n", s.sum;
