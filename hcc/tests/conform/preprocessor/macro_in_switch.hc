// macro_in_switch.hc — macros as case values

#include <stdio.hh>
#include <stdlib.hh>
#define CMD_START 1
#define CMD_STOP  2
#define CMD_RESET 3

U0 Run(I64 cmd) {
  switch (cmd) {
    case CMD_START: "start\n"; break;
    case CMD_STOP:  "stop\n"; break;
    case CMD_RESET: "reset\n"; break;
    default: "unknown\n";
  }
}
Run(CMD_START);
Run(CMD_STOP);
Run(CMD_RESET);
Run(99);
