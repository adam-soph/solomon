// build_string_in_buf.hc — write bytes into a buffer then print

#include <stdio.hh>
U8 buf[8];
buf[0] = 'f';
buf[1] = 'o';
buf[2] = 'o';
buf[3] = '!';
buf[4] = 0;
"%s\n", buf;
