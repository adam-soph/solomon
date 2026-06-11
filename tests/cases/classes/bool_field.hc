// bool_field.hc — class with Bool (I64) flags field
class Flags { I64 enabled; I64 visible; };
Flags f; f.enabled = 1; f.visible = 0;
if (f.enabled) "on\n"; else "off\n";
if (f.visible) "visible\n"; else "hidden\n";
