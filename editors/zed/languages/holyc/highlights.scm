; Tree-sitter highlight queries for HolyC (solomon) — Zed capture names.
; First match wins, so specific patterns precede the generic identifier rule.

; ---- comments ----
(comment) @comment

; ---- preprocessor ----
[
  "#include"
  "#define"
  "#undef"
  "#ifdef"
  "#ifndef"
  "#else"
  "#endif"
] @keyword

(preproc_define name: (identifier) @constant)
(preproc_undef name: (identifier) @constant)
(preproc_if name: (identifier) @constant)
(system_lib_string) @string

; ---- types ----
(primitive_type) @type.builtin
(type_identifier) @type
(class_specifier name: (identifier) @type)
(class_specifier base: (identifier) @type)

; ---- language constants ----
[
  (true)
  (false)
  (null)
] @constant.builtin

; ---- builtin functions (kept in sync with src/builtins.rs) ----
(call_expression
  function: (identifier) @function.builtin
  (#any-of? @function.builtin
    "Print" "StrPrint" "CatPrint" "MStrPrint" "Str2I64"
    "Abs" "Sqrt" "StrLen" "StrCmp" "StrCpy" "MAlloc" "Free" "StrCat"
    "MemCpy" "MemSet" "ToUpper" "ToLower" "Sin" "Cos" "Pow" "MemCmp"
    "Floor" "Ceil" "Round" "Exp" "Ln" "Tan" "StrFind" "ASin" "ACos"
    "ATan" "ATan2" "Log10" "StrNCmp" "StrNCpy" "Fabs" "Sign" "RandU64"))

; ---- functions ----
(function_declarator declarator: (identifier) @function)
(call_expression function: (identifier) @function)

; ---- labels ----
(goto_statement label: (identifier) @label)
(labeled_statement label: (identifier) @label)
(range_label) @keyword

; ---- keywords ----
[
  "if" "else" "while" "do" "for"
  "switch" "case" "default"
  "break" "continue" "return" "goto"
  "start" "end"
] @keyword

[
  "class" "union" "typedef" "sizeof" "asm"
] @keyword

(storage_class) @keyword

; ---- members & parameters ----
(field_expression field: (identifier) @property)
(field_declaration declarator: (identifier) @property)
(designated_initializer field: (identifier) @property)
(parameter_declaration declarator: (identifier) @variable.parameter)

; ---- literals ----
(number_literal) @number
(string_literal) @string
(char_literal) @string
(escape_sequence) @string.escape

; ---- operators ----
[
  "+" "-" "*" "/" "%"
  "=" "+=" "-=" "*=" "/=" "%=" "&=" "|=" "^=" "<<=" ">>="
  "==" "!=" "<" ">" "<=" ">="
  "&&" "||" "!"
  "&" "|" "^" "~" "<<" ">>"
  "++" "--"
  "->" "." "?"
] @operator

; ---- punctuation ----
[ ";" "," ":" ] @punctuation.delimiter
[ "(" ")" "{" "}" "[" "]" ] @punctuation.bracket

; ---- variables (generic fallback, must come last) ----
(identifier) @variable
