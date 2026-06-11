// exceptions.hc — try / catch / throw with the implicit task struct `Fs`.
//
// `throw expr;` raises a value and unwinds to the nearest enclosing `try { } catch { }`,
// skipping the rest of the unwound code (even across function calls). HolyC's `catch`
// takes no parameter: inside it the thrown value is `Fs->except_ch`, and
// `Fs->catch_except` is 1 while an exception is being handled, else 0.

// Validate a value, throwing a multi-character code on failure.
U0 Check(I64 age)
{
  if (age < 0)   throw('LOW');
  if (age > 150) throw('HIGH');
  "  age %d ok\n", age;
}

// Sum 1..n, but throw the running total the moment it exceeds `cap` — the throw
// unwinds out of both the loop and the function in one step.
I64 SumUntil(I64 n, I64 cap)
{
  I64 i, total = 0;
  for (i = 1; i <= n; i++) {
    total += i;
    if (total > cap) throw(total);
  }
  return total;
}

U0 Main()
{
  I64 ages[4];
  ages[0] = 30; ages[1] = -5; ages[2] = 200; ages[3] = 42;
  I64 i;

  "validating:\n";
  for (i = 0; i < 4; i++)
    try {
      Check(ages[i]);
    } catch {
      "  age %d rejected, code=%d\n", ages[i], Fs->except_ch;
    }

  // A throw that unwinds out of a loop inside a called function.
  try {
    I64 s = SumUntil(100, 20);
    "  full sum = %d\n", s; // unreached: 1+2+..+6 = 21 > 20
  } catch {
    "  capped at %d\n", Fs->except_ch;
  }

  // Nested try with a bare `throw;` that re-raises to the outer handler.
  try {
    try {
      throw(7);
    } catch {
      "  inner caught %d, re-raising\n", Fs->except_ch;
      throw;
    }
  } catch {
    "  outer caught %d (flag=%d)\n", Fs->except_ch, Fs->catch_except;
  }

  "flag now %d\n", Fs->catch_except;
}

Main;
