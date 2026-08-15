; JavaScript/JSX/TSX: functions, classes, methods, arrow functions, variable/const/let declarations

(function_declaration
  name: (identifier) @name) @definition

(class_declaration
  name: (identifier) @name) @definition

(method_definition
  name: (property_identifier) @name) @definition

; All const, let, var declarations (arrow functions, function expressions, factories, schemas)
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
    name: (identifier) @name)) @definition

(export_statement
  declaration: (lexical_declaration
    (variable_declarator
      name: (identifier) @name))) @definition

(export_statement
  declaration: (variable_declaration
    (variable_declarator
      name: (identifier) @name))) @definition
