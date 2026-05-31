/**
 * Tree-sitter grammar for HolyC, the language implemented by solomon.
 *
 * Scope is the HolyC subset solomon actually parses (see src/parser.rs): the
 * builtin scalar types, class/union aggregates, functions, the full statement
 * and expression set, function pointers, and the C-style preprocessor. The
 * grammar is intentionally permissive — its job is to produce a syntax tree
 * good enough for editor highlighting, not to reject invalid programs (sema
 * does that in the compiler).
 */

const PREC = {
  COMMA: -1,
  ASSIGN: 1,
  TERNARY: 2,
  OR: 3,
  AND: 4,
  BIT_OR: 5,
  BIT_XOR: 6,
  BIT_AND: 7,
  EQUAL: 8,
  RELATIONAL: 9,
  SHIFT: 10,
  ADD: 11,
  MULTIPLY: 12,
  CAST: 13,
  UNARY: 14,
  POSTFIX: 15,
  CALL: 16,
};

const commaSep = (rule) => optional(commaSep1(rule));
const commaSep1 = (rule) => seq(rule, repeat(seq(',', rule)));

module.exports = grammar({
  name: 'holyc',

  word: ($) => $.identifier,

  extras: ($) => [/\s|\\\r?\n/, $.comment],

  conflicts: ($) => [
    [$.type_identifier, $._expression],
    [$.class_specifier],
    [$.class_definition, $._type_specifier],
    [$.class_definition],
  ],

  supertypes: ($) => [$._statement, $._expression, $._type_specifier],

  rules: {
    source_file: ($) => repeat($._top_level_item),

    _top_level_item: ($) =>
      choice($.function_definition, $._statement),

    // ---- preprocessor ----
    preproc_include: ($) =>
      seq(
        '#include',
        field('path', choice($.string_literal, $.system_lib_string)),
      ),

    system_lib_string: ($) => token(seq('<', /[^>\n]*/, '>')),

    preproc_define: ($) =>
      seq(
        '#define',
        field('name', $.identifier),
        optional(field('parameters', $.preproc_params)),
        optional(field('value', $.preproc_arg)),
        token.immediate(/\r?\n/),
      ),

    preproc_params: ($) =>
      seq(
        token.immediate('('),
        commaSep(choice($.identifier, '...')),
        ')',
      ),

    preproc_undef: ($) =>
      seq('#undef', field('name', $.identifier), token.immediate(/\r?\n/)),

    preproc_arg: ($) => token(prec(-1, /([^\\\r\n]|\\\r?\n|\\.)+/)),

    preproc_if: ($) =>
      seq(
        choice('#ifdef', '#ifndef'),
        field('name', $.identifier),
        repeat($._top_level_item),
        optional($.preproc_else),
        '#endif',
      ),

    preproc_else: ($) =>
      seq('#else', repeat($._top_level_item)),

    // ---- declarations & definitions ----
    type_definition: ($) =>
      seq(
        'typedef',
        field('type', $._type_specifier),
        commaSep1(field('declarator', $._declarator)),
        ';',
      ),

    declaration: ($) =>
      seq(
        repeat($.storage_class),
        field('type', $._type_specifier),
        commaSep(field('declarator', choice($._declarator, $.init_declarator))),
        ';',
      ),

    // A class/union definition. Unlike a normal declaration its trailing `;` is
    // optional (HolyC accepts `class Foo {...}` with or without it). Restricted
    // to a class/union type so it never competes with scalar declarations.
    class_definition: ($) =>
      prec.dynamic(
        1,
        seq(
          repeat($.storage_class),
          field('type', $.class_specifier),
          commaSep(field('declarator', choice($._declarator, $.init_declarator))),
          optional(';'),
        ),
      ),

    // A function definition's declarator must be a function declarator (possibly
    // wrapped in pointers, e.g. `U8 *Foo()`), never a bare identifier — that
    // keeps `class Name {...};` from being misread as a nameless-type function.
    function_definition: ($) =>
      seq(
        repeat($.storage_class),
        field('type', $._type_specifier),
        field('declarator', $._function_declarator_decl),
        field('body', $.compound_statement),
      ),

    _function_declarator_decl: ($) =>
      choice($.function_declarator, $.pointer_declarator, $.parenthesized_declarator),

    storage_class: ($) =>
      choice('extern', 'public', 'import', 'reg', 'noreg', '_extern', 'lastclass'),

    init_declarator: ($) =>
      seq(field('declarator', $._declarator), '=', field('value', $._initializer)),

    _declarator: ($) =>
      choice(
        $.pointer_declarator,
        $.function_declarator,
        $.array_declarator,
        $.parenthesized_declarator,
        $.identifier,
      ),

    pointer_declarator: ($) => prec.right(seq('*', field('declarator', $._declarator))),

    parenthesized_declarator: ($) => seq('(', $._declarator, ')'),

    function_declarator: ($) =>
      prec(1, seq(field('declarator', $._declarator), field('parameters', $.parameter_list))),

    array_declarator: ($) =>
      prec(1, seq(field('declarator', $._declarator), '[', optional($._expression), ']')),

    parameter_list: ($) =>
      seq('(', commaSep(choice($.parameter_declaration, $.variadic_parameter)), ')'),

    variadic_parameter: () => '...',

    parameter_declaration: ($) =>
      seq(field('type', $._type_specifier), optional(field('declarator', $._declarator))),

    // ---- types ----
    _type_specifier: ($) =>
      choice($.primitive_type, $.class_specifier, $.type_identifier),

    primitive_type: () =>
      token(choice('U0', 'I8', 'U8', 'I16', 'U16', 'I32', 'U32', 'I64', 'U64', 'F64', 'Bool')),

    type_identifier: ($) => alias($.identifier, $.type_identifier),

    // Two forms: a *definition* (carries a body, possibly anonymous) and a bare
    // *reference* (`union Reg r;`). The bodied form has higher dynamic precedence
    // so a `{` after a class name always attaches as the body rather than being
    // mistaken for a following block statement.
    class_specifier: ($) =>
      choice(
        prec.dynamic(
          2,
          seq(
            field('kind', choice('class', 'union')),
            optional(field('name', $.identifier)),
            optional(seq(':', field('base', $.identifier))),
            field('body', $.field_declaration_list),
          ),
        ),
        prec.dynamic(
          1,
          seq(
            field('kind', choice('class', 'union')),
            field('name', $.identifier),
            optional(seq(':', field('base', $.identifier))),
          ),
        ),
      ),

    field_declaration_list: ($) => seq('{', repeat($.field_declaration), '}'),

    field_declaration: ($) =>
      seq(
        field('type', $._type_specifier),
        commaSep(field('declarator', $._declarator)),
        ';',
      ),

    // ---- statements ----
    compound_statement: ($) => seq('{', repeat($._statement), '}'),

    _statement: ($) =>
      choice(
        $.preproc_include,
        $.preproc_define,
        $.preproc_undef,
        $.preproc_if,
        $.compound_statement,
        $.class_definition,
        $.declaration,
        $.type_definition,
        $.expression_statement,
        $.if_statement,
        $.while_statement,
        $.do_statement,
        $.for_statement,
        $.switch_statement,
        $.case_statement,
        $.range_label,
        $.return_statement,
        $.break_statement,
        $.continue_statement,
        $.goto_statement,
        $.labeled_statement,
        $.asm_statement,
        $.empty_statement,
      ),

    empty_statement: () => ';',

    expression_statement: ($) => seq(choice($._expression, $.comma_expression), ';'),

    if_statement: ($) =>
      prec.right(
        seq(
          'if',
          field('condition', $.parenthesized_expression),
          field('consequence', $._statement),
          optional(seq('else', field('alternative', $._statement))),
        ),
      ),

    while_statement: ($) =>
      seq('while', field('condition', $.parenthesized_expression), field('body', $._statement)),

    do_statement: ($) =>
      seq(
        'do',
        field('body', $._statement),
        'while',
        field('condition', $.parenthesized_expression),
        ';',
      ),

    for_statement: ($) =>
      seq(
        'for',
        '(',
        choice(
          field('initializer', $.declaration),
          seq(field('initializer', optional(choice($._expression, $.comma_expression))), ';'),
        ),
        field('condition', optional($._expression)),
        ';',
        field('update', optional(choice($._expression, $.comma_expression))),
        ')',
        field('body', $._statement),
      ),

    switch_statement: ($) =>
      seq(
        'switch',
        field('value', choice($.parenthesized_expression, $.bracketed_expression)),
        field('body', $.compound_statement),
      ),

    bracketed_expression: ($) => seq('[', $._expression, ']'),

    case_statement: ($) =>
      prec.right(
        seq(
          choice(
            seq(
              'case',
              field('value', $._expression),
              optional(seq('...', field('end', $._expression))),
            ),
            'default',
          ),
          ':',
          repeat($._statement),
        ),
      ),

    range_label: ($) => seq(field('label', choice('start', 'end')), ':'),

    return_statement: ($) => seq('return', optional($._expression), ';'),
    break_statement: () => seq('break', ';'),
    continue_statement: () => seq('continue', ';'),
    goto_statement: ($) => seq('goto', field('label', $.identifier), ';'),

    labeled_statement: ($) =>
      prec.right(seq(field('label', $.identifier), ':', optional($._statement))),

    asm_statement: ($) => seq('asm', $.compound_statement),

    // ---- expressions ----
    _expression: ($) =>
      choice(
        $.identifier,
        $.number_literal,
        $.string_literal,
        $.char_literal,
        $.true,
        $.false,
        $.null,
        $.call_expression,
        $.subscript_expression,
        $.field_expression,
        $.unary_expression,
        $.binary_expression,
        $.update_expression,
        $.assignment_expression,
        $.conditional_expression,
        $.cast_expression,
        $.sizeof_expression,
        $.pointer_expression,
        $.parenthesized_expression,
      ),

    comma_expression: ($) =>
      prec.left(PREC.COMMA, seq($._expression, ',', choice($._expression, $.comma_expression))),

    parenthesized_expression: ($) =>
      seq('(', choice($._expression, $.comma_expression), ')'),

    _initializer: ($) => choice($._expression, $.initializer_list),

    initializer_list: ($) =>
      seq('{', commaSep(choice($._initializer, $.designated_initializer)), optional(','), '}'),

    designated_initializer: ($) =>
      seq('.', field('field', $.identifier), '=', field('value', $._initializer)),

    call_expression: ($) =>
      prec(PREC.CALL, seq(field('function', $._expression), field('arguments', $.argument_list))),

    argument_list: ($) => seq('(', commaSep($._expression), ')'),

    subscript_expression: ($) =>
      prec(PREC.CALL, seq(field('argument', $._expression), '[', field('index', $._expression), ']')),

    field_expression: ($) =>
      prec(
        PREC.CALL,
        seq(field('argument', $._expression), field('operator', choice('.', '->')), field('field', $.identifier)),
      ),

    update_expression: ($) =>
      prec.right(
        PREC.UNARY,
        choice(
          seq(field('operator', choice('++', '--')), field('argument', $._expression)),
          prec(PREC.POSTFIX, seq(field('argument', $._expression), field('operator', choice('++', '--')))),
        ),
      ),

    unary_expression: ($) =>
      prec.right(PREC.UNARY, seq(field('operator', choice('!', '~', '-', '+')), field('argument', $._expression))),

    pointer_expression: ($) =>
      prec.right(PREC.UNARY, seq(field('operator', choice('*', '&')), field('argument', $._expression))),

    sizeof_expression: ($) =>
      prec.right(
        PREC.UNARY,
        seq('sizeof', choice(seq('(', $.type_descriptor, ')'), $._expression)),
      ),

    cast_expression: ($) =>
      prec.right(PREC.CAST, seq('(', field('type', $.type_descriptor), ')', field('value', $._expression))),

    type_descriptor: ($) => seq($._type_specifier, repeat('*')),

    assignment_expression: ($) =>
      prec.right(
        PREC.ASSIGN,
        seq(
          field('left', $._expression),
          field('operator', choice('=', '+=', '-=', '*=', '/=', '%=', '&=', '|=', '^=', '<<=', '>>=')),
          field('right', $._expression),
        ),
      ),

    conditional_expression: ($) =>
      prec.right(
        PREC.TERNARY,
        seq(
          field('condition', $._expression),
          '?',
          field('consequence', $._expression),
          ':',
          field('alternative', $._expression),
        ),
      ),

    binary_expression: ($) => {
      const table = [
        ['||', PREC.OR],
        ['&&', PREC.AND],
        ['|', PREC.BIT_OR],
        ['^', PREC.BIT_XOR],
        ['&', PREC.BIT_AND],
        ['==', PREC.EQUAL],
        ['!=', PREC.EQUAL],
        ['<', PREC.RELATIONAL],
        ['>', PREC.RELATIONAL],
        ['<=', PREC.RELATIONAL],
        ['>=', PREC.RELATIONAL],
        ['<<', PREC.SHIFT],
        ['>>', PREC.SHIFT],
        ['+', PREC.ADD],
        ['-', PREC.ADD],
        ['*', PREC.MULTIPLY],
        ['/', PREC.MULTIPLY],
        ['%', PREC.MULTIPLY],
      ];
      return choice(
        ...table.map(([op, p]) =>
          prec.left(
            p,
            seq(field('left', $._expression), field('operator', op), field('right', $._expression)),
          ),
        ),
      );
    },

    // ---- literals & tokens ----
    identifier: () => /[A-Za-z_][A-Za-z0-9_]*/,

    number_literal: () =>
      token(
        choice(
          /0[xX][0-9A-Fa-f]+/,
          /0[bB][01]+/,
          /[0-9]+\.[0-9]+([eE][+-]?[0-9]+)?/,
          /[0-9]+[eE][+-]?[0-9]+/,
          /[0-9]+/,
        ),
      ),

    string_literal: ($) => seq('"', repeat(choice($.escape_sequence, $._string_content)), '"'),
    _string_content: () => token.immediate(prec(1, /[^"\\\n]+/)),

    char_literal: ($) => seq("'", repeat(choice($.escape_sequence, $._char_content)), "'"),
    _char_content: () => token.immediate(prec(1, /[^'\\\n]+/)),

    escape_sequence: () =>
      token.immediate(seq('\\', choice(/[abfnrtv0\\"'?]/, /x[0-9A-Fa-f]{1,2}/, /[0-7]{1,3}/))),

    true: () => 'TRUE',
    false: () => 'FALSE',
    null: () => 'NULL',

    comment: () =>
      token(
        choice(seq('//', /[^\n]*/), seq('/*', /[^*]*\*+([^/*][^*]*\*+)*/, '/')),
      ),
  },
});
