// multi_elif.hc — #elif chain with defined flags (grade selection)
#define GRADE_B
// GRADE_A, GRADE_C, GRADE_D not defined

#if defined(GRADE_A)
"A\n";
#elif defined(GRADE_B)
"B\n";
#elif defined(GRADE_C)
"C\n";
#elif defined(GRADE_D)
"D\n";
#else
"F\n";
#endif
