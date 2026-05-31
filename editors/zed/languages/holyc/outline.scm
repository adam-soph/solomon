; Symbol outline for the Zed outline panel / breadcrumbs.

(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @item

(function_definition
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (identifier) @name))) @item

(class_definition
  type: (class_specifier
    kind: _ @context
    name: (identifier) @name)) @item

(type_definition
  declarator: (identifier) @name) @item

(field_declaration
  declarator: (identifier) @name) @item
