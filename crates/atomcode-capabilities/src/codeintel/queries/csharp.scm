; C#: full symbol outline for classes, records, interfaces, structs, enums,
; methods, constructors, properties, events, delegates, namespaces, locals.

(class_declaration
  name: (identifier) @name) @definition

(record_declaration
  name: (identifier) @name) @definition

(interface_declaration
  name: (identifier) @name) @definition

(struct_declaration
  name: (identifier) @name) @definition

(enum_declaration
  name: (identifier) @name) @definition

(enum_member_declaration
  name: (identifier) @name) @definition

(method_declaration
  name: (identifier) @name) @definition

(constructor_declaration
  name: (identifier) @name) @definition

(destructor_declaration
  name: (identifier) @name) @definition

(property_declaration
  name: (identifier) @name) @definition

(event_declaration
  name: (identifier) @name) @definition

(delegate_declaration
  name: (identifier) @name) @definition

(local_function_statement
  name: (identifier) @name) @definition

; Namespaces — bare identifier form (MyNs) and simple qualified form (A.B).
(namespace_declaration
  name: (identifier) @name) @definition

(namespace_declaration
  name: (qualified_name) @name) @definition

(file_scoped_namespace_declaration
  name: (identifier) @name) @definition

(file_scoped_namespace_declaration
  name: (qualified_name) @name) @definition
