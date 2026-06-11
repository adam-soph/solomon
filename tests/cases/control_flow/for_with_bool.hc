// Bool condition in for loop.
Bool found = FALSE;
I64 i;
for (i = 0; i < 20 && !found; i++) {
  if (i * i > 50)
    found = TRUE;
}
"i=%d found=%d\n", i, found;
