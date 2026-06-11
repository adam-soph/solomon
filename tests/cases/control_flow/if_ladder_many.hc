// Multi-branch if/else ladder.
U0 Grade(I64 score)
{
  if (score >= 90)
    "A\n";
  else if (score >= 80)
    "B\n";
  else if (score >= 70)
    "C\n";
  else if (score >= 60)
    "D\n";
  else
    "F\n";
}
Grade(95);
Grade(83);
Grade(71);
Grade(65);
Grade(42);
