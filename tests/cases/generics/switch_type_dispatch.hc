// switch_type_dispatch.hc — switch type dispatch
U8 *TypeName<type T>() {
  switch type (T) {
    case I64:   return "I64";
    case F64:   return "F64";
    case U8 *:  return "U8*";
    default:    return "other";
  }
}
"%s\n", TypeName<I64>();
"%s\n", TypeName<F64>();
"%s\n", TypeName<U8 *>();
