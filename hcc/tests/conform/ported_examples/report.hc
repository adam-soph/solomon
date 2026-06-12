// report.hc — build a formatted sales report in a heap buffer using StrPrint
// (write) and CatPrint (append), then print it. Exercises printf-style column
// alignment (`%-10s`, `%4d`), fixed-point money (`%8.2f`), and accumulation —
// all rendered identically by the interpreter and the native backend.



#include <stdio.hh>
#include <stdlib.hh>
#include <string.hh>
class Item {
  U8 *name;
  I64 qty;
  F64 price;
}

U0 Main() {
  Item items[4];
  items[0].name = "Widget";   items[0].qty = 4;  items[0].price = 2.50;
  items[1].name = "Gizmo";    items[1].qty = 10; items[1].price = 1.25;
  items[2].name = "Sprocket"; items[2].qty = 2;  items[2].price = 9.99;
  items[3].name = "Cog";      items[3].qty = 25; items[3].price = 0.40;

  U8 *out = MAlloc(1024);
  StrPrint(out, "%-10s %4s %8s %9s\n", "Item", "Qty", "Price", "Total");
  CatPrint(out, "%-10s %4s %8s %9s\n", "----", "---", "-----", "-----");

  F64 grand = 0.0;
  I64 units = 0;
  I64 i;
  for (i = 0; i < 4; i++) {
    F64 total = items[i].qty * items[i].price;
    grand = grand + total;
    units = units + items[i].qty;
    CatPrint(out, "%-10s %4d %8.2f %9.2f\n",
             items[i].name, items[i].qty, items[i].price, total);
  }
  CatPrint(out, "%-10s %4d %8s %9.2f\n", "TOTAL", units, "", grand);

  "%s", out;
  "(%d bytes)\n", StrLen(out);
  Free(out);
}

Main;
