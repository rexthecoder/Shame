; Shame's additions on top of tree-sitter-python's highlights query.
; Later patterns win, so these refine the upstream captures.

((identifier) @variable.builtin
 (#match? @variable.builtin "^(self|cls)$"))

(class_definition
  name: (identifier) @type)

((type (identifier) @type.builtin)
 (#match? @type.builtin "^(int|str|float|bool|bytes|list|dict|set|tuple|object|complex|type)$"))

(decorator "@" @attribute)
(decorator (identifier) @attribute)

(keyword_argument
  name: (identifier) @property)

["(" ")" "[" "]" "{" "}"] @punctuation.bracket
["," "." ":" ";"] @punctuation.delimiter
