" vim syntax highlighting for pith, derived from the textmate grammar
" in tooling/highlighting/pith.tmLanguage.json. keep the two in sync
" when the language grows a keyword.

if exists("b:current_syntax")
  finish
endif

" declarations and modifiers
syn keyword pithDeclaration fn struct enum interface impl type import from as pub mut ref weak
" control flow and concurrency
syn keyword pithControl if elif else for in while match return fail break continue spawn await select defer errdefer catch timeout default test
" word operators
syn keyword pithWordOperator and or not
" literals
syn keyword pithBoolean true false
syn keyword pithNone none
" builtin types (pascal-case user types get their own match below)
syn keyword pithBuiltinType Int UInt Float Bool String Bytes Void Task Channel List Map Set Option Result Error
syn keyword pithBuiltinType Int8 Int16 Int32 Int64 UInt8 UInt16 UInt32 UInt64 Float32 Float64

" a pascal-case identifier reads as a type
syn match pithUserType "\<[A-Z][A-Za-z0-9_]*\>"

" function name in a declaration
syn match pithFunctionDecl "\<fn\s\+\zs[a-z_][a-zA-Z0-9_]*"

" comments: # to end of line; doc comments often use ##
syn match pithComment "#.*$" contains=pithTodo
syn keyword pithTodo TODO FIXME XXX contained

" numbers, with underscore separators
syn match pithNumber "\<\d[0-9_]*\>"
syn match pithFloat "\<\d[0-9_]*\.\d[0-9_]*\%([eE][+-]\?\d[0-9_]*\)\?\>"
syn match pithHex "\<0x[0-9A-Fa-f][0-9A-Fa-f_]*\>"
syn match pithBinary "\<0b[01][01_]*\>"
syn match pithOctal "\<0o[0-7][0-7_]*\>"

" strings: escapes, {interpolation} with {{ as a literal brace
syn region pithString start=+"+ skip=+\\"+ end=+"+ contains=pithEscape,pithInterpolation
syn match pithEscape "\\[nrt\"{0\\]" contained
syn region pithInterpolation matchgroup=pithInterpolationBrace start="{" end="}" contained contains=pithNumber,pithWordOperator
syn match pithEscapedBrace "{{" contained containedin=pithString

" operators
syn match pithOperator ":=\|+=\|-=\|\*=\|/=\|->\|=>\|==\|!=\|<=\|>="

hi def link pithDeclaration Keyword
hi def link pithControl Statement
hi def link pithWordOperator Operator
hi def link pithBoolean Boolean
hi def link pithNone Constant
hi def link pithBuiltinType Type
hi def link pithUserType Type
hi def link pithFunctionDecl Function
hi def link pithComment Comment
hi def link pithTodo Todo
hi def link pithNumber Number
hi def link pithFloat Float
hi def link pithHex Number
hi def link pithBinary Number
hi def link pithOctal Number
hi def link pithString String
hi def link pithEscape SpecialChar
hi def link pithInterpolationBrace Special
hi def link pithEscapedBrace SpecialChar
hi def link pithOperator Operator

let b:current_syntax = "pith"
