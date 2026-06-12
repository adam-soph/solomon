
#include <math.hh>
#include <stdio.hh>
I64 q;
"%g %d\n", Remquo(7.0, 2.0, &q), q;      // n = RoundToEven(3.5) = 4, r = -1
"%g %d\n", Remquo(-7.0, 2.0, &q), q;
"%g %d\n", Remquo(17.5, 4.0, &q), q;
"%g %d\n", Remquo(1.0, 8.0, &q), q;      // rounds to 0
"%g %d\n", Remquo(100.0, 3.0, &q), q;    // n = 33, low 3 bits = 1
"%d %d\n", IsNaN(Remquo(1.0, 0.0, &q)), q;
"%g %g\n", Scalbn(1.5, 4), Scalbln(3.0, -1);
"%d\n", Nexttoward(1.0, 2.0) == Nextafter(1.0, 2.0);
