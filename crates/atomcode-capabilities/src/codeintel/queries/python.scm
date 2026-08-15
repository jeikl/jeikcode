; Python: functions, classes, methods, decorators, top-level assignments

(function_definition
  name: (identifier) @name) @definition

(class_definition
  name: (identifier) @name) @definition

(decorated_definition
  definition: (function_definition
    name: (identifier) @name)) @definition

(decorated_definition
  definition: (class_definition
    name: (identifier) @name)) @definition

(assignment
  left: (identifier) @name) @definition

