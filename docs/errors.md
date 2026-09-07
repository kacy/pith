# error codes

pith diagnostics use stable error codes grouped by compiler phase.
codes are never reused — if a code is retired, it stays retired.

use `pith check --json <file>` or `pith lint --json <file>` for
machine-readable output that includes the error code, location, message,
and fix suggestion (if available).

---

## lexing and parsing errors (E24x)

the lexer and parser do not have their own numeric ranges. every diagnostic
they produce lands in the E24x block alongside the checker's, and the four
codes below are the whole set:

| code | phase | what it covers |
|------|-------|----------------|
| `E240` | parser | an expected token was missing |
| `E241` | parser | the file ended in the middle of a construct |
| `E242` | parser | the tokens do not form a valid construct here |
| `E243` | lexer | an invalid token: a stray character, an unterminated string, or a bad indentation change |

each is documented in full below. codes in the E0xx and E1xx ranges have never
been emitted and are not reserved for these phases.

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

**not currently emitted.** rebinding a name in the same scope is accepted; there is no duplicate-definition check.

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

**not currently emitted.** a repeated match arm is rejected by the parser as `E240` instead.

### E206 — missing return type

a function needs a return type annotation but doesn't have one.

**not currently emitted.** a function with no return type annotation is treated as returning nothing.

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

the same code covers a missing method, and a method call that writes type
arguments on a name the receiver's type does not declare as a method with
type parameters — a builtin such as `s.len[Int]()`, or a field.

```
error[E209]: String has no method 'len' that takes type arguments
```

### E210 — not a struct type

a field access or struct constructor was used on a non-struct type. the same
code reports an explicit type argument that is not a type at all, such as
`Channel[1](2)` or `Holder[f()]()`; a type written in expression position may
be a name, a parameterized type, an optional, a tuple of those, or an
associated type of a type parameter (`engine[A.Msg]()`).

### E211 — not an enum type

an enum variant pattern was used on a non-enum type.

**not currently emitted.** matching an enum pattern against a non-enum is rejected by the parser as `E240` instead.

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
a builtin container position that looks a value up rather than stores it —
`xs.contains(3)`, `m.contains_key(3)`, a map key — reports here too, because
a freshly built `Some(3)` would not compare equal to the one the container
holds; see `docs/limitations.md`.

### E220 — pipe operator error

the right side of a pipe operator (`|`) is not a valid function.

**not currently emitted.** a bad right-hand side to `|` reports `E208` instead.

### E221 — generic type argument count mismatch

a generic type was used with the wrong number of type arguments. the channel
constructor counts too: a channel carries exactly one payload type, and
`Channel[Int, String](1)` used to build a `Channel[Int]` silently, dropping
the rest. send a struct when one message should carry several fields.

```
error[E221]: Channel expects 1 type argument, got 2
```

type arguments written at a method call, `x.describe[Int](v)`, count
against the method's own bracket list the same way; a method that declares
no type parameters expects zero.

```
error[E221]: describe expects 1 type arguments, got 2
```

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

if/elif/else branches, match arms or select arms produce different types when
the whole is used as an expression. a `none` beside a value is not a mismatch:
the branches settle on the value's optional, so `match v: none => none; x => x`
is a `T?`, and a value beside its own optional settles on the optional the same
way.

### E226 — interface constraint violation

a type doesn't satisfy the interface bounds required by a generic parameter.

### E227 — method not found

a method call references a method that doesn't exist on the type.

**not currently emitted.** a missing method reports `E209` instead, which covers both fields and methods.

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

**not currently emitted.** importing a name a module does not declare reports `E246` instead.

### E237 — imported name is not public

a `from ... import` refers to a name that exists but isn't marked `pub`.

```
error[E237]: 'secret' is not public in the imported module
  1 | from math import secret
                       ^^^^^^
```

**not currently emitted**, and there is no check behind it: `from mod import name` currently succeeds whether or not `name` is `pub`. only the `mod.name(...)` call form is gated, by `E251`.

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

a spawned task runs concurrently with the scope that spawned it, on a pool of
os threads. reference counts are atomic, but a list, map, or set is an
unsynchronized buffer behind a handle, so passing one the current scope still
holds lets both mutate the same buffer.
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

reported by `pith lint`. all of them are warnings: `pith lint` reports them
without failing a build.

### E300 — snake_case required (warning)

function names, variable names, and method names must use `snake_case`.

```
error[E300]: function name 'GetUser' should be snake_case
  1 | fn GetUser():
       ^^^^^^^
```

### E301 — PascalCase required (warning)

type names (structs, enums, interfaces, type aliases) must use `PascalCase`.

```
error[E301]: struct name 'my_point' should be PascalCase
  1 | struct my_point:
            ^^^^^^^^
```

### E304 — missing doc comment (warning)

a public function or method has no doc comment. every `pub fn` should have a
`#` comment on the line directly above it explaining its purpose. there is no
`///` form — that is a parse error.

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

### E306 — strong reference cycle (warning)

a struct holds a strong reference back to itself, directly (`next: Node?`) or
through a chain of fields across other structs in the same module. a cycle of
strong references keeps its own count above zero, so it is never reclaimed;
mark the back edge `weak` (see [ownership.md](ownership.md)). a `weak` field
is not an edge, and neither is a function-typed field. a cycle that runs
through another module's structs is not visible to the linter and passes.

```
warning[E306]: struct 'Node' holds a strong reference to itself through field 'next'; a cycle of strong references is never reclaimed
  3 |     next: Node?
          ^
  fix: mark the back edge with `weak` (`weak next: ...`), see docs/ownership.md
```

### E307 — resource never closed (warning)

a local binding holds a resource its caller owns and nothing in the function
closes it. reference counting reclaims the handle's memory but not the registry
slot behind it, so the slot is held until the process exits with no other
diagnostic. pair the build with a `defer` close (see [defer.md](defer.md)), or
close it explicitly.

the constructor list is empty at the moment. it held the three `std.net.tls`
config builders, and a tls config now implements `Drop` (docs/ownership.md,
"destructors"): a config built and never closed gives its slot back when the
last value naming it goes away, so the shape this rule reported is no longer a
leak. the rule stays for the std types that still close by hand, which
docs/destructors_roadmap.md lists; an entry is added when one of them is
audited onto the list rather than the destructor.

the rule is deliberately narrow, because a false positive on a resource that is
closed elsewhere is worse than no rule. it reports only when every later mention
of the name is the receiver of a method that is not a closer. passing the value
as an argument, returning it, assigning it, storing it, reading a field off it,
or capturing it in a lambda all silence the rule — each of those can hand the
resource to something the linter cannot see. it also follows `!` and a chain of
builder methods back to the constructor, so
`tls.client_config()!.with_alpn(["h2"])` is recognized.

the constructor list is short on purpose. an entry has to be a call whose result
the caller owns, on a type with no method that consumes the receiver instead of
closing it — `bytes.ByteBuffer` is excluded for exactly that reason, since
`take_bytes()` frees the buffer as it takes its contents. the survey behind the
list is in [destructors_roadmap.md](destructors_roadmap.md).

the message names the constructor, as it did for a tls config before the
destructor:

```
warning[E307]: 'config' is built by tls.client_config() and never closed; the resource behind the handle is not reclaimed when the binding goes out of scope
  7 |     config := tls.client_config()!
                                       ^
  fix: close it on every exit -- `defer config.close()` next to the build; see docs/destructors_roadmap.md
```

### E266 — module global read before a local of the same name

a function's storage is flat: one slot per name for the whole body. a local, a
parameter or a `match` / `if let` payload therefore takes its name for the
entire function, and the module global of that name is unreachable from inside
it. that is legal on its own (the linter mentions it as
[E308](#e308--binding-shadows-a-module-global-warning)). what has no answer is a
body that wants both — reads the global somewhere above and binds a local of the
same name below.

rename the binding, or move the reads of the global into a function that does
not bind the name.

a `for` variable is exempt. it is the one binding whose storage is scoped to a
block rather than the function, so the global is still reachable on either side
of the loop and nothing is ambiguous.

### E267 — a generic instantiated at more than 64 distinct type sets

a generic body is checked once per distinct set of concrete types the program
asks for (see docs/generics.md). a generic that calls itself at a larger type
(`fn grow[T](x: T)` calling `grow([x])`) asks for `T`, then `List[T]`, then
`List[List[T]]`, without end. the checker stops queueing the declaration after
64 sets and reports it once.

```
error[E267]: 'grow' is instantiated at more than 64 distinct type sets; a generic that calls itself at a larger type never stops
```

not reported when the check is switched off (`PITH_CHECK_GENERIC_BODIES=off`
or `silent`); the declaration is then left unchecked past the cap, as every
generic body was before the check existed.

### E268 — invalid Drop implementation

`impl Drop for T` asks the compiler to call `T`'s `drop` method from the
destructor it attaches to every `T` it builds, when the last value naming the
struct goes away (see docs/ownership.md, "destructors"). the impl has to be a
shape that destructor can call: `T` must be a struct, declared in the module
the impl is written in, and not generic — the destructor is generated per
declaration and spells the method symbol from the declaring module, and a
generic struct's destructor is generated per instance with no place for the
call. the impl must declare `fn drop()` with no parameters and no return
value, since the runtime hands it the bare pointer and reads nothing back, and
nothing else beside it.

```
error[E268]: impl Drop for Guard: a generic struct cannot implement Drop
error[E268]: impl Drop for Shape: only a struct can implement Drop
error[E268]: impl Drop for Slot: drop() takes no parameters
error[E268]: impl Drop for Slot: the impl may only declare drop(), not 'close'
error[E268]: impl Drop for Slot: the impl must declare fn drop()
```

`Drop` is a language-level interface: nothing declares it and nothing imports
it. put the type's other methods in an `impl T:` block of their own, and have
`drop` call the explicit closer so the two stay idempotent.

before globals were given a storage namespace of their own, this compiled and
miscompiled: the binding wrote the GLOBAL's slot, so the local and the global
were one value and the global's next reader anywhere in the program saw
whatever the local had left there.

```
error[E266]: 'items' names a module global that is read earlier in this function, and this binding takes the name for the whole function
 10 |     mut items: List[Int] := [7]
          ^
  fix: rename the binding, or move the reads of the global into a function that does not bind the name
```

### E308 — binding shadows a module global (warning)

a local, a parameter or a `match` / `if let` payload is spelled like one of the
module's globals. the binding reads its own storage, so the global is
unreachable for the rest of that function — every mention of the name inside is
the binding, whichever was declared first. a `for` variable is exempt: its
storage lasts the loop and the global is still reachable on either side of it.

that is legal and sometimes deliberate, which is why it is a warning. it is
worth a look anyway: a reader cannot tell a function that means to read the
global from one that means to hold its own value, and the two are one rename
apart.

it used to be a miscompile rather than a readability problem. the ir namespace
is flat and the consumer resolved a bare `load`/`store` operand to a global's
data slot whenever one existed, so the binding wrote the GLOBAL's storage: the
global's own next read came back as whatever the binding had left there, and a
container global reached `list indexing on invalid list handle` once the
binding's cleanup freed what the slot still named. globals now carry a storage
symbol of their own (`__g_<name>`) and a binding keeps its written name, so the
two can no longer meet.

```
warning[E308]: binding 'items' shadows the module global declared on line 1; the global is unreachable for the rest of this function
  4 |     mut items: List[Int] := [7, 8, 9]
          ^
  fix: rename the binding, or drop the global if the binding is what you meant
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


### E254 — unhashable set element or map key

a `Set` element or a `Map` key must hash, and a set and a map hash exactly
three flavors: the int family, `String`, and `Bytes`, which is hashed and
compared by content. every other type used to fall into the string flavor
anyway and misbehave silently — an optional's shell and a list's allocation
header were read as a c-string, collapsing distinct values into one entry; a
struct compared raw memory bytes; a plain enum inserted nothing at all; a
float crashed the process. the type is rejected wherever it is formed: a
written annotation, an inferred literal, an empty literal's first typed
store, and a generic instantiation. store `List[T?]` instead of `Set[T?]`,
and key by a string, integer or bytes encoding of the value otherwise (an id
field for a struct, a serialized form for a tuple). an optional map value
stays legal; only the key is hashed.

```
error[E254]: an optional cannot be a set element type; store List[T?] or key by the payload instead
error[E254]: a list cannot be a map key type; a set element and a map key hash int, string and bytes flavors only
```


### E260 — invalid defer or errdefer

`defer` and `errdefer` appear outside a function, or the deferred statement is
not a plain side effect. a deferred statement may not return, `fail`, `break`,
`continue`, use `!` or `?`, bind a name, or nest another defer — move the
control flow into a helper and defer the call to it. `errdefer` additionally
requires a function that returns a result (`T!`), since there is no error case
for it to run on otherwise.

```
error[E260]: errdefer is only meaningful in a function returning a result (`T!`)
  fix: use `defer` for cleanup that must always run, or give the function a `T!` return type
```

see [defer.md](defer.md) for the full ordering rules.

### E261 — invalid weak binding

a `weak` binding broke one of its restrictions. a weak binding cannot be `mut`,
cannot carry a type annotation (it takes its type from its value), must hold a
struct value, cannot be declared inside a closure, and cannot reuse a name
already bound in the same function.

```
error[E261]: a weak binding cannot be 'mut'
```

see [ownership.md](ownership.md) for why each restriction exists.

### E262 — generic enum instance cannot be inferred

a payload-free variant of a generic enum carries nothing to infer the type
argument from. bound without an annotation, the value would have no concrete
type — and a later match on it could not be checked. annotate the binding;
a constructor with a payload argument (`Opt.Some(5)`) infers its instance
and needs no annotation.

```
error[E262]: cannot infer the type arguments of generic enum 'Opt' from this constructor; annotate the binding
```


### E263 — duplicate method across impl blocks

two impl blocks gave the same declaration the same method name with the
same kind of declaration, so one body would silently shadow the other and
which one answered depended on module order. remove or rename one of the
two. an interface impl re-declaring a method the inherent impl also has is
not a duplicate — that pair is the conformance idiom std/io uses — and an
impl in another module adding new method names to a type it can see stays
legal too.

```
error[E263]: duplicate method 'read' on 'FileStream': another impl block already declares it
```

### E264 — where clause names an undeclared type parameter

a `where` clause put a bound on a name that is not one of the function's
declared type parameters. the clause form and the inline form
(`[T: Display]`) are two spellings of the same bounds, so a clause can
only constrain names the bracket list declares. declare the parameter,
or fix the spelling.

```
error[E264]: where clause names 'U', which is not a type parameter of 'f'
```

### E265 — generic method reuses its owner's type parameter name

a method of a generic type declared a type parameter with the same name as
one of the type's own. the two lists compose by name — the type's parameters
are fixed by the receiver's instance, the method's by the call — so a
method parameter spelled `T` on a `Box[T]` would resolve to the box's `T`
everywhere it is written, never to the call's type. give the method's
parameter a name of its own.

the code used to refuse every generic method on a generic type; that shape
is supported now (docs/generics.md), and only the name clash is reported.

```
error[E265]: generic method 'describe' on generic type 'Box' reuses its owner's type parameter name 'T'; a method's own type parameters need names of their own
```
