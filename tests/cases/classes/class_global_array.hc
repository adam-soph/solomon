// class_global_array.hc — global array of class, iterate and sum
class Item { I64 price; I64 qty; };
Item items[4];
I64 i;
for (i = 0; i < 4; i++) { items[i].price = (i + 1) * 5; items[i].qty = i + 1; }
I64 total = 0;
for (i = 0; i < 4; i++) total = total + items[i].price * items[i].qty;
"%d\n", total;
