// char_range_loop.hc — loop over a range of characters
I64 c = 'A';
while (c <= 'E') {
    "%c", c;
    c++;
}
"\n";
c = '0';
while (c <= '4') {
    "%c", c;
    c++;
}
"\n";
