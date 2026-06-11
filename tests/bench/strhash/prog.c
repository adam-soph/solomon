#include <stdio.h>
int main(void){
  const char *s = "the quick brown fox jumps over the lazy dog 0123456789";
  long long h = 5381;
  for (long long rep = 0; rep < 200000; rep++) {
    for (long long i = 0; s[i]; i++) h = ((h << 5) + h + s[i]) & 0x7FFFFFFF;
  }
  printf("%lld\n", h);
  return 0;
}
