; C# call-site extraction for the cross-file code graph.
; Captures the callee name (method or constructed type) as @callee.

; Foo()
(invocation_expression
  function: (identifier) @callee)

; obj.Foo() / Type.Foo()
(invocation_expression
  function: (member_access_expression
    name: (identifier) @callee))

; Foo<T>() / obj.Foo<T>()
(invocation_expression
  function: (generic_name
    (identifier) @callee))

(invocation_expression
  function: (member_access_expression
    name: (generic_name
      (identifier) @callee)))

; new Foo() / new Foo<T>()
(object_creation_expression
  type: (identifier) @callee)

(object_creation_expression
  type: (generic_name
    (identifier) @callee))

(object_creation_expression
  type: (qualified_name
    name: (identifier) @callee))
