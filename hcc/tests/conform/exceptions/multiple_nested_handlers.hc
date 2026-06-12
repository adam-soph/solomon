// multiple_nested_handlers.hc — three levels of try/catch

#include <stdio.hh>
try {
  try {
    try {
      throw(1);
    } catch {
      "level3 %d\n", Fs->except_ch;
      throw(2);
    }
  } catch {
    "level2 %d\n", Fs->except_ch;
    throw(3);
  }
} catch {
  "level1 %d\n", Fs->except_ch;
}
"done\n";
