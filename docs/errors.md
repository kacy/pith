# error codes

pith diagnostics use stable error codes grouped by compiler phase.
codes are never reused — if a code is retired, it stays retired.

use `pith check --json <file>` or `pith lint --json <file>` for
machine-readable output that includes the error code, location, message,
and fix suggestion (if available).

---

## lexer errors (E0xx)

### E001 — unexpected character

the lexer encountered a character it doesn't recognize.

```
error[E001]: unexpected character: @
  1 | x := @bad
          ^
```

### E002 — unterminated string

a string literal was opened but never closed.

```
error[E002]: unterminated string literal
  1 | x := "hello
            ^^^^^^
```

### E003 — invalid escape sequence

a backslash in a string is followed by an unrecognized escape character.

```
error[E003]: invalid escape sequence: \q
  1 | x := "hello\q"
                  ^^
```

### E004 — invalid number literal

a numeric literal has invalid syntax (e.g. multiple dots, letters in the middle).

### E005 — indentation error

the indentation level is inconsistent or uses mixed tabs/spaces.

```
error[E005]: inconsistent indentation
  3 |     x := 1
       ^^^^
  fix: use consistent 4-space indentation
```

### E006 — string interpolation error

a `{` inside a string interpolation is malformed or unmatched.

---

## parser errors (E1xx)

### E100 — unexpected token

the parser encountered a token it didn't expect in the current context.

```
error[E100]: unexpected token: ')'
  5 | fn add(x: Int, ) -> Int:
                      ^
```

### E101 — expected expression

the parser expected an expression but found something else.

```
error[E101]: expected expression
  3 | x :=
          ^
```

### E102 — expected type annotation

a type annotation was expected (e.g. after `:` in a parameter or field).

```
error[E102]: expected type annotation
  1 | fn foo(x: ) -> Int:
                ^
```

### E103 — expected identifier

an identifier was expected but not found (e.g. after `fn` or in a binding).

### E104 — expected block

a colon-terminated statement expected an indented block to follow.

```
error[E104]: expected indented block
  1 | fn foo():
  2 | bar()
      ^^^^^
```

### E105 — expression nesting too deep

an expression exceeds the maximum nesting depth (prevents stack overflow
on malicious or generated input).

### E106 — invalid lambda syntax

a lambda expression has invalid syntax (e.g. missing `=>` or body).

### E107 — expected pattern

a pattern was expected in a match arm but something else was found.

---

## checker errors (E2xx)

### E200 — type mismatch

an expression's type doesn't match what was expected. this is the most
common checker error. includes a fix suggestion when the mismatch is in
a return type.

```
error[E200]: type mismatch: expected String, got Int
  3 |   x * 2
      ^^^^^ this is Int, but the function returns String
  fix: change the return type to Int
```

a bare `none` reports here too. `none` is the empty case of an optional and
satisfies nothing else, so it is accepted only where the target is already
a `T?` — a binding, an assignment, a return, an argument, a struct field, a
collection element, a tuple element, or the other side of `==` / `!=`.

```
error[E200]: return type mismatch: expected Int, got none
  8 |     return none
             ^
  fix: return a value, or declare the return type Int?
```

### E201 — undefined variable

a variable or function name was used but never defined.

```
error[E201]: undefined variable 'foo'
  5 |   print(foo)
              ^^^
```

`mod.name(...)` reaches only what `mod` itself declares. every module's
top-level names live in one shared table, and without this check the lookup
falls through to a function some unrelated module declared, then emits a call
that resolves to nothing.

a method is an easy way to land here. a method is not a module function and has
no free-function spelling: `wait` on `Process` is written `proc.wait()`, never
`process.wait(proc)`. when the first argument's type has a method by that name,
the error says so.

```
error[E201]: undefined function: process.wait
  fix: 'wait' is a method on Process; call it as receiver.wait(...)
```

### E202 — undefined type

a type name was used in an annotation but doesn't exist.

```
error[E202]: undefined type 'Foob'
  1 | fn bar(x: Foob) -> Int:
                ^^^^
```

### E203 — duplicate definition

a name was defined more than once in the same scope.

```
error[E203]: duplicate definition of 'x'
  3 | x := 10
      ^
```

### E204 — non-exhaustive match

a match expression doesn't cover all possible values. includes a fix
suggestion listing the missing patterns.

```
error[E204]: non-exhaustive match: missing variant 'Circle'
  5 | match shape:
      ^^^^^
  fix: add missing arm: Circle(..)
```

### E205 — unreachable pattern

a match arm can never be reached because earlier arms already cover it.

### E206 — missing return type

a function needs a return type annotation but doesn't have one.

### E207 — wrong number of arguments

a function call has too many or too few arguments.

```
error[E207]: expected 2 arguments, got 3
  5 | add(1, 2, 3)
      ^^^^^^^^^^^
```

### E208 — not callable

an expression was used as a function call but its type isn't callable.

### E209 — field not found

a field access references a field that doesn't exist on the struct.

```
error[E209]: field 'z' not found on type 'Point'
  3 | p.z
        ^
```

### E210 — not a struct type

a field access or struct constructor was used on a non-struct type.

### E211 — not an enum type

an enum variant pattern was used on a non-enum type.

### E212 — unknown variant

an enum variant name doesn't exist on the enum type.

### E213 — wrong field count in pattern

a pattern has the wrong number of fields for the enum variant.

### E214 — reserved

reserved for future checker diagnostics. not currently emitted.

### E215 — break/continue outside loop

a `break` or `continue` statement was used outside of a loop body.

### E216 — assignment to immutable binding

a variable was assigned to but wasn't declared with `mut`. includes
a fix suggestion.

```
error[E216]: cannot assign to immutable variable 'x'
  3 | x = 10
      ^
  fix: declare with 'mut': mut x := ...
```

### E217 — invalid operand types

an operator was used with types that don't support it.

```
error[E217]: operator '+' not supported for types Bool and Bool
  2 | true + false
      ^^^^^^^^^^
```

### E218 — match guard must be Bool

the `if` guard on a match arm doesn't evaluate to Bool.

### E219 — argument type mismatch

a function argument has the wrong type.

```
error[E219]: argument type mismatch: expected String, got Int
  3 | greet(42)
            ^^
```

a bare `none` passed to a parameter that is not an optional reports here.

```
error[E219]: argument type mismatch: expected Int, got none
  11 |     print(takes_int(none).to_string())
                 ^
```

the reverse is fine: a plain value passed to an optional parameter widens to
`Some(v)`, so `f(3)` against `fn f(x: Int?)` is accepted. it does not run
backwards — an `Int?` argument to an `Int` parameter still reports here, and
so does an inner type that does not match (`f("three")` against `Int?`).
a few argument positions do not widen yet; see `docs/limitations.md`.

### E220 — pipe operator error

the right side of a pipe operator (`|`) is not a valid function.

### E221 — generic type argument count mismatch

a generic type was used with the wrong number of type arguments.

### E222 — generic type inference failure

the compiler couldn't infer the type arguments for a generic type.

### E223 — collection type inference error

the compiler couldn't determine the element type of a collection literal,
or the elements disagree. a `none` element is folded in rather than compared
— `[none, 1]` is a `List[Int?]` — but a set element and a map key are hashed
and compared, so neither accepts `none`.

```
error[E223]: set element cannot be none
  4 |     zs: Set[Int] := {none}
                           ^
```

### E224 — invalid unwrap/try target

the `?` (unwrap) operator was used on a non-optional type, or the `!`
(try) operator was used on a non-result type. includes a fix suggestion.

```
error[E224]: try requires a result type, got Int
  3 | x!
      ^^
  fix: use ? for unwrapping optional types
```

### E225 — branch type mismatch

if/elif/else branches return different types when used as an expression.

### E226 — interface constraint violation

a type doesn't satisfy the interface bounds required by a generic parameter.

### E227 — method not found

a method call references a method that doesn't exist on the type.

### E228 — pattern type mismatch

a pattern in a match arm doesn't match the type being matched on.

### E229 — invalid self usage

`self` was used outside of a method body. includes a fix suggestion.

```
error[E229]: 'self' can only be used inside a method body
  1 | fn foo(): self.x
                ^^^^
  fix: define methods inside an 'impl' block with 'self' as the first parameter
```

### E230 — missing type annotation

a type annotation is required but wasn't provided.

### E231 — return outside function

a `return` statement was used outside of a function body.

```
error[E231]: return statement outside of function
  1 | return 42
      ^^^^^^
  fix: 'return' can only be used inside a function body
```

### E232 — spawn/await type error

a `spawn` or `await` expression has a type error.

### E233 — type nesting too deep

type declarations are nested too deeply (exceeds the compiler's recursion limit).
this usually happens with deeply nested generic types or recursive type definitions.

```
error[E233]: type nesting too deep
```

### E234 — invalid fail target

the `fail` statement was used in a function that does not return a result type.

### E235 — import cycle detected

two or more modules import each other, forming a cycle. break the cycle
by restructuring the code or extracting shared types into a third module.

### E236 — imported name not found

a `from ... import` refers to a name that doesn't exist in the module.

```
error[E236]: name 'subtract' not found in the imported module
  1 | from math import subtract
                       ^^^^^^^^
```

### E237 — imported name is not public

a `from ... import` refers to a name that exists but isn't marked `pub`.

```
error[E237]: 'secret' is not public in the imported module
  1 | from math import secret
                       ^^^^^^
```

### E238 — invalid unwrap context

the `?` operator was used outside a function that returns a result type.
`?` propagates `none` as a `fail`, so the enclosing function must be able
to carry that error. Either change the function to return `T!`, or replace
`?` with `match`/`unwrap_or` for an in-place default.

```
error[E238]: unwrap (?) can only be used in functions that return a result type
  3 | print("got: {value?}")
                  ^^^^^^
  fix: wrap main or the enclosing fn return in T! and propagate, or replace ? with `match`/`unwrap_or` for an in-place default
```

### E239 — invalid try context

the `!` operator was used inside a function that does not itself return a result type.

### E240 — expected token

the parser expected a specific token but encountered a different one.

### E241 — unexpected end of file

the parser reached the end of the file before a required token or block terminator appeared.

### E242 — invalid syntax

the parser encountered a token sequence that does not form a valid expression,
pattern, or declaration.

### E243 — lexer error

the lexer produced an invalid token — an unterminated string literal, a
stray character that starts no valid token, or a bad indentation change.
every lexer error is reported, even one that lands between two otherwise
valid tokens (`1 € + 2`), so an invalid character can never be silently
dropped.


### E245 — invalid if let / while let pattern

`if let` and `while let` take a variant pattern (which destructures like a
match arm) or a bare binding (which unwraps an optional subject). anything
else — a literal pattern, or a bare binding against a non-optional — is
rejected.

```
error[E245]: if let with a bare binding needs an optional subject, got Int
```

### E246 — import of an undeclared name

a `from` import must name something its source module actually declares
at the top level. a wrong module name in an import otherwise compiles
clean and dies much later as a silent unknown call in the backend.

```
error[E246]: 'imaginary_helper' is not declared by module 'helper'
```

### E247 — cannot find module

an `import` or `from ... import` names a module whose file cannot be
found. standard-library modules are imported with the `std.` prefix
(`import std.fs as fs`, not `import fs`); local modules resolve
relative to the importing file.

```
error[E247]: cannot find module 'fs'
  1 | import fs
             ^
```

### E248 — collection shared into a spawned task

a spawned task runs on its own os thread. reference counts are atomic, but
a list, map, or set is an unsynchronized buffer behind a handle, so passing
one the current scope still holds lets both threads mutate the same buffer.
pass data through a channel, hand the task a copy, or use an atomic cell for
a shared scalar. a fresh collection — a literal, a copy, a call result — is
not shared and is allowed.

```
error[E248]: a spawned task cannot take a list this scope still holds; both threads would mutate the same buffer
  6 |     t := spawn worker(data)
                            ^
```

---

### E249 — weak field must be an optional struct

```
error[E249]: weak field 'parent' must be an optional struct type (T?)
```

a `weak` field points at a struct without keeping it alive, so it must be able
to read back as "gone" once the target is reclaimed — which means it has to be
an optional struct type, `T?`. `weak parent: Node` is rejected; write
`weak parent: Node?`. weak is for breaking reference cycles (a parent/child back
pointer, a doubly-linked list's back link); see docs/ownership.md.

### E250 — builtin type name

a struct or enum cannot reuse the name of a builtin type (`Channel`, `Task`,
`List`, `Map`, `Mutex`, `WaitGroup`, `Semaphore`, `AtomicInt`, or a primitive
like `Int`). the declaration would shadow the builtin and silently miscompile
anything that uses it — a `struct Channel`, for instance, corrupts every
`Channel[T]` in scope, so an imported module's own channels stop working with
no error. rename the type.

```
error[E250]: 'Channel' is a builtin type name and cannot be used as a struct name
  2 |     n: Int
                ^
```

---

## lint errors (E3xx)

reported by `pith lint`. naming violations are errors; style issues are warnings.

### E300 — snake_case required (error)

function names, variable names, and method names must use `snake_case`.

```
error[E300]: function name 'GetUser' should be snake_case
  1 | fn GetUser():
       ^^^^^^^
```

### E301 — PascalCase required (error)

type names (structs, enums, interfaces, type aliases) must use `PascalCase`.

```
error[E301]: struct name 'my_point' should be PascalCase
  1 | struct my_point:
            ^^^^^^^^
```

### E302 — unused variable (warning)

a local variable is bound but never referenced in its scope.

```
warning[E302]: unused variable 'x' in 'main'
  3 |     x := 42
        ^
```

### E304 — missing doc comment (warning)

a public function or method has no doc comment. every `pub fn` should have
a `///` doc comment explaining its purpose.

```
warning[E304]: public function 'serve' is missing a doc comment
  5 | pub fn serve():
            ^^^^^
```

### E305 — deep nesting (warning)

code is indented more than 4 levels deep. consider extracting a helper
function to reduce complexity.

```
warning[E305]: indentation depth 5 exceeds maximum of 4
  12 |                     if x > 0:
                            ^^
```

### E251 — function is not public in that module

a module function declared with a bare `fn` belongs to the module that declared
it; only a `pub fn` can be called through an import. the two live in one shared
name table, so before this check the lookup for `mod.helper` would fall through
to a bare-name match and return something the caller had no business holding.

mark the function `pub` if it is meant to be part of the module's surface, or
call the public entry point that wraps it.

```
error[E251]: 'frame_message' is not public in module 'grpc'
 11 |     framed := grpc.frame_message(bytes.from_string_utf8("hi"))
                                                                    ^
```

### E252 — this file has no main function

`pith build` and `pith run` make an executable, and an executable needs an
entry point. a file without a top-level `fn main` is a library: it can be
imported, checked and linted, but not built on its own. before this check the
build sailed through to the system linker, which answered with an
undefined-reference dump instead of a diagnosis.

```
error[E252]: this file has no main function
  fix: `pith build` and `pith run` make an executable. a module without main can be imported, checked and linted, but not built on its own
```


### E253 — catch block must leave

the block form of `catch` runs on the error path and produces no value, so
falling out of its end would leave the surrounding binding with nothing.
end the block with `return`, `fail`, `continue` or `break`.

```
error[E253]: a catch block must end with return, fail, continue or break; it produces no value for the surrounding expression
```
