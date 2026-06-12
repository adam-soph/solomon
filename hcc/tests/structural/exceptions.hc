
#include <stdio.hh>
#include <stdlib.hh>
U0 Boom() { throw(7); }
try { Boom(); } catch { "caught %d\n", Fs->except_ch; }
