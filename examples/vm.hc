// vm.hc — a tiny stack machine. A program is a flat array of (opcode, argument)
// pairs; Run executes it with a switch over opcodes. Exercises a class with an
// array field, passing arrays to pointer parameters, nested case blocks, and
// helper calls that mutate state through a pointer.

#define OP_HALT 0
#define OP_PUSH 1
#define OP_ADD  2
#define OP_SUB  3
#define OP_MUL  4
#define OP_NEG  5

#define STACK_MAX 32

class VM {
  I64 stack[STACK_MAX];
  I64 sp;
};

U0 Push(VM *vm, I64 v) {
  vm->stack[vm->sp] = v;
  vm->sp++;
}

I64 Pop(VM *vm) {
  vm->sp--;
  return vm->stack[vm->sp];
}

// Execute `len` cells of `code` (opcode/argument pairs); returns the top of
// stack.
I64 Run(I64 *code, I64 len) {
  VM vm;
  vm.sp = 0;
  I64 ip = 0;
  while (ip < len) {
    I64 op = code[ip];
    I64 arg = code[ip + 1];
    ip += 2;
    switch (op) {
      case OP_PUSH:
        Push(&vm, arg);
        break;
      case OP_ADD: {
        I64 b = Pop(&vm);
        I64 a = Pop(&vm);
        Push(&vm, a + b);
        break;
      }
      case OP_SUB: {
        I64 b = Pop(&vm);
        I64 a = Pop(&vm);
        Push(&vm, a - b);
        break;
      }
      case OP_MUL: {
        I64 b = Pop(&vm);
        I64 a = Pop(&vm);
        Push(&vm, a * b);
        break;
      }
      case OP_NEG:
        Push(&vm, -Pop(&vm));
        break;
      case OP_HALT:
        return Pop(&vm);
    }
  }
  return Pop(&vm);
}

U0 Main() {
  // Evaluate -( (2 + 3) * 4 - 5 ) = -15
  I64 prog[16];
  prog[0] = OP_PUSH;  prog[1] = 2;
  prog[2] = OP_PUSH;  prog[3] = 3;
  prog[4] = OP_ADD;   prog[5] = 0;
  prog[6] = OP_PUSH;  prog[7] = 4;
  prog[8] = OP_MUL;   prog[9] = 0;
  prog[10] = OP_PUSH; prog[11] = 5;
  prog[12] = OP_SUB;  prog[13] = 0;
  prog[14] = OP_NEG;  prog[15] = 0;

  I64 result = Run(prog, 16);
  "vm result = %d\n", result;
}

Main;
