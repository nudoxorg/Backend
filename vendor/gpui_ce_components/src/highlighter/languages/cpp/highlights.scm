; NUDOX: tree-sitter-cpp's own query only adds C++ to tree-sitter-c's (it is
; written for `; inherits: c`, which this highlighter does not resolve). This
; is both, specific patterns first: the highlighter keeps the first capture
; of a node.

; Functions

(call_expression
  function: (qualified_identifier
    name: (identifier) @function))

(call_expression
  function: (identifier) @function)

(call_expression
  function: (field_expression
    field: (field_identifier) @function))

(template_function
  name: (identifier) @function)

(template_method
  name: (field_identifier) @function)

(function_declarator
  declarator: (qualified_identifier
    name: (identifier) @function))

(function_declarator
  declarator: (field_identifier) @function)

(function_declarator
  declarator: (identifier) @function)

(preproc_function_def
  name: (identifier) @function.macro)

; Types

((namespace_identifier) @type
 (#match? @type "^[A-Z]"))

(namespace_identifier) @namespace

(auto) @type
(type_identifier) @type
(primitive_type) @type.builtin
(sized_type_specifier) @type.builtin

; Constants

(this) @variable.builtin
(null) @constant
(true) @boolean
(false) @boolean
(number_literal) @number
(char_literal) @string

((identifier) @constant
 (#match? @constant "^[A-Z][A-Z\\d_]*$"))

; Strings and comments

(string_literal) @string
(raw_string_literal) @string
(system_lib_string) @string
(escape_sequence) @string.escape
(comment) @comment

; Keywords

[
  "break"
  "case"
  "const"
  "continue"
  "default"
  "do"
  "else"
  "enum"
  "extern"
  "for"
  "if"
  "inline"
  "return"
  "sizeof"
  "static"
  "struct"
  "switch"
  "typedef"
  "union"
  "volatile"
  "while"
  "catch"
  "class"
  "co_await"
  "co_return"
  "co_yield"
  "constexpr"
  "constinit"
  "consteval"
  "delete"
  "explicit"
  "final"
  "friend"
  "mutable"
  "namespace"
  "noexcept"
  "new"
  "override"
  "private"
  "protected"
  "public"
  "template"
  "throw"
  "try"
  "typename"
  "using"
  "concept"
  "requires"
  "virtual"
  "#define"
  "#elif"
  "#else"
  "#endif"
  "#if"
  "#ifdef"
  "#ifndef"
  "#include"
] @keyword

(preproc_directive) @keyword

; Punctuation and operators

[
  "--" "-" "-=" "->" "=" "!=" "*" "&" "&&" "+" "++" "+=" "<" "==" ">" "||"
  "!" "~" "|" "^" "%" "/" "::" "<<" ">>" "<=" ">="
] @operator

[ "." ";" "," ":" "(" ")" "[" "]" "{" "}" ] @punctuation

; Plain

(field_identifier) @property
(statement_identifier) @label
(identifier) @variable
