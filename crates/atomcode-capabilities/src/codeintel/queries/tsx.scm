; TypeScript: functions, classes, interfaces, type aliases, enums, variable/const/let declarations

(function_declaration
  name: (identifier) @name) @definition

(class_declaration
  name: (type_identifier) @name) @definition

(interface_declaration
  name: (type_identifier) @name) @definition

(type_alias_declaration
  name: (type_identifier) @name) @definition

(enum_declaration
  name: (identifier) @name) @definition

(method_definition
  name: (property_identifier) @name) @definition

; All const, let, var declarations (arrow functions, Effect.fn, schemas, factories, objects)
(lexical_declaration
  (variable_declarator
    name: (identifier) @name)) @definition

(variable_declaration
  (variable_declarator
    name: (identifier) @name)) @definition

(export_statement
  declaration: (function_declaration
    name: (identifier) @name)) @definition

(export_statement
  declaration: (class_declaration
    name: (type_identifier) @name)) @definition

(export_statement
  declaration: (interface_declaration
    name: (type_identifier) @name)) @definition

(export_statement
  declaration: (type_alias_declaration
    name: (type_identifier) @name)) @definition

(export_statement
  declaration: (enum_declaration
    name: (identifier) @name)) @definition

(export_statement
  declaration: (lexical_declaration
    (variable_declarator
      name: (identifier) @name))) @definition

(export_statement
  declaration: (variable_declaration
    (variable_declarator
      name: (identifier) @name))) @definition




; JSX/TSX UI elements (buttons, components, dialogs, titles) — classified as
; UiElement in the code graph so natural-language UI queries can recall them.
; The opening tag's name is an `identifier` (e.g. `<el-button>` → "el-button");
; `<Foo.Bar>` uses a member_expression. Both pair and self-closing forms.
(jsx_element
  (jsx_opening_element
    name: (identifier) @name)) @definition

(jsx_element
  (jsx_opening_element
    name: (member_expression) @name)) @definition

(jsx_self_closing_element
  name: (identifier) @name) @definition

(jsx_self_closing_element
  name: (member_expression) @name) @definition
