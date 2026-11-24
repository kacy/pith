# error codes

pith diagnostics use stable error codes grouped by compiler phase.
codes are never reused — if a code is retired, it stays retired.

use `pith check --json <file>` or `pith lint --json <file>` for
machine-readable output that includes the error code, location, message,
and fix suggestion (if available).

---

## lexing and parsing errors (E24x)

the lexer and parser do not have their own numeric ranges. general lex and
parse diagnostics land in the E24x block alongside the checker's, covered by
the four codes below (the parser also emits `E264` for malformed `where`
clauses and `E274` for malformed `guard` blocks):

| code | phase | what it covers |
|------|-------|----------------|
| `E240` | parser | an expected token was missing |
| `E241` | parser | the file ended in the middle of a construct |
| `E242` | parser | the tokens do not form a valid construct here |
| `E243` | lexer | an invalid token (a stray character, an unterminated string, or a bad indentation change), a file over 16 MiB, or a file that is not valid utf-8 |

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

a function declared `-> T?!` (the same type as `(T?)!`) accepts a plain `T`,
`none` or a `T?` as its ok value: `return name` with `name: String` widens to
`Some(name)` inside the ok result, the way it does in a `-> T?` function.

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

**not currently emitted.** rebinding a name in the same scope is accepted; there is no duplicate-definition check. rebinding it inside a nested block reports `E262`.

### E204 — non-exhaustive match

a match expression doesn't cover all possible values. includes a fix
suggestion listing the missing patterns. a guarded arm never counts toward
coverage, since its guard can be false: `_ if n > 0 => ...` alone is not
exhaustive. a binding on an optional subject covers the present values only,
so the match also needs a `none` arm.

```
error[E204]: non-exhaustive match: missing variant 'Circle'
  5 | match shape:
      ^^^^^
  fix: add missing arm: Circle(..)
```

### E205 — unreachable pattern

a match arm can never be reached because an earlier arm without a guard is a
wildcard, a binding, or a tuple of those, and matches every value. a binding
on an optional subject matches only present values, so a `none` arm after it
is still reachable. a repeated arm that is not a catch-all is not reported.

```
error[E205]: unreachable match arm: an earlier arm without a guard matches every value
  4 |         1 => 1
              ^
  fix: remove this arm, or move it above the catch-all arm
```

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

**not currently emitted.** matching an enum pattern against a non-enum reports `E228` instead.

### E212 — unknown variant

an enum variant name doesn't exist on the enum type. the check covers a
payload-free variant (`Color.Teal`), a variant constructor (`Shape.Square(2)`)
and a variant pattern in a match or `if let`, for an enum declared locally,
reached through a module alias (`m.Color.Teal`) or imported under another name.
the caret points at the variant name. a call that matches neither a variant
nor a method of the enum reports "has no variant or method".

```
error[E212]: Color has no variant 'Teal'
  6 |     c := Color.Teal
                     ^
```

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

`==` and `!=` on a list, map or set report here too. on a collection they
would compare the handles rather than the contents:

```
error[E217]: operator == is not defined on a list: it would compare identity, not contents
  4 |     print("{a == b}")
               ^
  fix: compare the contents with `lists_equal(a, b)` from std.collections, or `assert_eq(a, b)` in a test
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

the compiler couldn't infer the type arguments for a generic function or
type. a generic struct constructor takes its type arguments from its
arguments, or else from the type of the place it is stored into: an annotated
binding, a return, a parameter, a field, an element of a typed literal, or an
assignment. `b: Bag[String] := Bag()` builds a `Bag[String]`, while `b :=
Bag()` with nothing to fix `T` is refused, since its `= {}` and `= []`
defaults would be built over an unknown type and own nothing stored into
them.

```
error[E222]: cannot infer type parameter T of 'Bag' from this constructor
  13 |     b := Bag()
                ^
  fix: name the type, as Bag[...](), or store the value where its type is declared
```

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
a type implements an interface only through an `impl Iface for T` block;
methods with the right names and signatures are not enough, so a struct
with a bare `fn next() -> Int?` can drive a `for` loop but cannot be passed
to `std.iter.to_list`. the fix line names the missing impl.

```
error[E226]: type 'Counter' does not implement interface 'Iterator'
  19 |     print("{to_list(Counter(cur: 0, hi: 3))}")
                   ^
  fix: add `impl Iterator for Counter:`. methods with the right names do not satisfy an interface on their own
```

### E227 — method not found

a method call references a method that doesn't exist on the type.

**not currently emitted.** a missing method reports `E209` instead, which covers both fields and methods.

### E228 — pattern type mismatch

a pattern in a match arm doesn't match the type being matched on. a bare
name that equals a variant of the matched enum is also E228: a bare name is a
binding that matches every value, so the variant has to be written qualified.

```
error[E228]: 'Red' is a variant of Color, but a bare name in a pattern is a binding that matches every value
  8 |         Red => 1
              ^
  fix: write Color.Red to match the variant
```

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


a spawn's arguments and a method call's receiver are evaluated on the new task, so
`!` among them has no function to return from. bind the value before the spawn:

```
value := load()!
t := spawn work(value)
```

### E233 — type nesting too deep

type declarations are nested too deeply (exceeds the compiler's recursion limit).
this usually happens with deeply nested generic types or recursive type definitions.

```
error[E233]: type nesting too deep
```

### E234 — invalid fail target

the `fail` statement was used in a function that does not return a result type.

### E235 — import cycle or missing interface method

two or more modules import each other, forming a cycle. break the cycle
by restructuring the code or extracting shared types into a third module.

the checker also emits `E235` when an `impl Interface for Type` block omits a
required method declared by the interface (`impl of <Iface> for <Type> is
missing method: <name>`).

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

**not currently emitted**, and there is no check behind it: `from mod import name` currently succeeds whether or not `name` is `pub`. module-qualified access (`mod.name` / `mod.name(...)`), private struct fields outside their declaring module, and internal `byte_buffer_*` builtins are gated by `E251`.

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

a statement ends at the line end, so a second expression on the same line is
an error rather than a second statement. a call needs its parentheses:

```
error[E240]: expected a line end after `assert` but found identifier
  6 |     assert x == 4
          ^
  fix: to call `assert`, put its arguments in parentheses: `assert(...)`
```

### E241 — unexpected end of file

the parser reached the end of the file before a required token or block terminator appeared.

### E242 — invalid syntax

the parser encountered a token sequence that does not form a valid expression,
pattern, or declaration. a bare name alone as a statement, such as `x` on its
own line, is reported here too: it has no effect.

it also reports input nested past the parser's limits: expressions, types or
patterns more than 256 levels deep, an expression tree deeper than 25,000
levels (a chain of 25,000 operators or about 8,000 calls), or blocks nested
more than 1,000 deep. see the compiler input limits in `SECURITY.md`.

### E243 — lexer error

the lexer produced an invalid token — an unterminated string literal, a
stray character that starts no valid token, a bad indentation change, or a
`0x`, `0b` or `0o` prefix with no digits after it.
every lexer error is reported, even one that lands between two otherwise
valid tokens (`1 € + 2`), so an invalid character can never be silently
dropped.

a source file over 16 MiB, or one that is not valid utf-8, is reported with
this code too. the utf-8 report gives the byte offset of the first invalid
byte.

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

the caret points at the undeclared name, wherever it sits in the list.

```
error[E246]: 'imaginary_helper' is not declared by module 'helper'
  1 | from helper import imaginary_helper, real_helper
                         ^
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

### E248 — a value shared with another task

a spawned task runs concurrently with the scope that spawned it, on a pool of
os threads. reference counts are atomic, but a list, map, or set is an
unsynchronized buffer behind a handle, so passing one the current scope still
holds lets both mutate the same buffer. the same holds for a value sent
through a channel: the receiver gets the handle, not a copy, so `send` and
`try_send` are checked like a spawn's arguments. a tuple, an optional or an
enum payload holding a collection counts as the collection.

a value may cross only when nothing else holds what it holds:

- a value built in the call is fresh when what it is built from is: a
  literal, a struct or an enum variant built from fresh parts, or the result
  of a function whose result holds no buffer of its arguments, of a global or
  of anything the checker cannot follow. `Box(items: xs)` holds `xs`, and so
  does the result of a function that returns its parameter or builds a struct
  around it. a copy is fresh as far as it copies: `copy_list` of a
  `List[Int]` is fresh, while a copy of a `List[List[Int]]` shares the inner
  lists.
- a local is moved when the scope does not use it again and nothing else
  holds it: no other local, field or closure, and no loop or closure around
  the handoff that the local was declared outside of, since that hands the
  same buffer over once per pass. a use in another branch of the same `if` or
  `match`, or after a `return` that follows the handoff, does not count; a
  use in a closure, a `defer` or an `errdefer` does, wherever it is written.
- a parameter can be moved the same way. the function then keeps what it is
  handed, and each call to it is judged as a handoff of its argument.

a spawn evaluates its arguments on the new task, so an argument that reads a
local, such as `spawn total(copy_list(xs))`, reads `xs` there while this
scope may change it. it is refused unless the local is moved or nothing
changes it; make the copy before the spawn and hand over the copy. a field
path or an element (`acct.inbox`, `xs[0]`) is judged by its type, since the
value that holds it still does.

an interface value can hold any type that implements its interface, so a held
one is refused when any implementing type reaches a collection outside a
`guard` block, even if the value at hand holds a type that does not. the
message names the type and field (`Bag.items`). a conversion written in the
call (`spawn work(Sq(s: 2))`) is fresh and allowed.

`std.bytes.ByteBuffer` and `ByteArray` count as collections under this rule,
even though their handle is an `Int`. neither can be converted to an interface
value (such as `io.BytesWriter`), because that would hide the buffer from these
checks (`E248`). use `io.memory_writer()` when an in-memory `BytesWriter` needs
to cross tasks or become an interface value, since its buffer sits behind a
lock.

these rules apply in a program that spawns, as E309 defines it. that includes
a program whose only spawn is in a function it reaches through a function
value (`f := lib.launch`, then `f(xs)`).

```
error[E248]: a spawned task cannot take this value: it shares a buffer with 'data', which this scope uses again, and both tasks would reach it
  6 |     t := spawn worker(data)
                            ^
  fix: hand over a value nothing else holds: build it in the call, or move a local the scope does not use again
```

```
error[E248]: a spawned task evaluates its arguments itself, so this one reads a buffer it shares with 'xs', which this scope uses again, while this scope may still change it
  10 |     t := spawn total(copy_list(xs))
                                     ^
  fix: compute the value before the spawn and hand over the result, or move a local the scope does not use again
```

a module global is refused too, and the message names it. the whole program
holds a global, so a task handed one shares its buffer with every other task
that reads it. a task can read the global itself without taking it as an
argument, and keep a global that tasks write in a `guard` block
(docs/concurrency.md).

```
error[E248]: a spawned task cannot take the module global 'items', a list every task can reach; both would mutate the same buffer
  10 |     t := spawn total(items)
                            ^
  fix: let the task read the global itself, and keep a global that tasks write in a `guard` block
```

a struct argument is refused the same way when one of its fields reaches a
list, a map or a set outside a `guard` block, directly or through a nested
struct, an optional, a tuple or an enum payload. move the field into a
`guard` block, or build the struct from fresh parts in the call, or move a
local holding it, so the other task holds the only reference.

```
error[E248]: a spawned task cannot take this Account: its field 'inbox.messages' is a collection outside a `guard` block, and both threads would reach the same buffer
  16 |     t := spawn count(acct)
                            ^
  fix: move the field into a `guard` block, or hand the task its own copy
```

a struct is also refused when one of its fields holds a counted value and
some code reassigns that field. a counted value is a string, bytes, a struct,
an optional, a tuple, a function value, an interface value, an enum with a
payload, or a handle such as a `Channel`, a `Mutex` or a `Task`, and the field
holds it as one pointer. a task that assigns the field releases the old value
while another task reading the field may be taking its own count on it, which
frees the value twice or reads freed memory. any field can be assigned, so the
checker looks at whether some code does.

- a field is settled when no code anywhere in the program assigns it after
  its struct is built: no `x.f = v`, `x.f += v` or `self.f = v`, in any
  module. the receiver's type decides, so a write through any value of the
  struct counts, and a write through one instance of a generic struct counts
  for every instance. a settled field keeps the value it was built with, and
  any task may read it, as with a `frozen` field.
- a field in a `guard` block is reached under its lock and does not count.
- a `weak` field does not count. its load and its store each run as one
  step, under the runtime's weak lock once another task can reach the
  struct, so a read never sees a target the store is freeing.
- a field holding a number, a `Bool` or a tag-only enum does not count. two
  tasks writing one race on the value, which is a logic error, but no memory
  is freed.

the walk is the one used for collections: struct fields, optional and
collection elements, tuple elements, enum payloads, and every type that
implements an interface value's interface. the message names the field path
and the first assignment the program makes to that field.

```
error[E248]: a spawned task cannot take this Holder: its field 'name' is reassigned at app.pith:9, and a write while the other task reads it frees the value being read
  17 |     a := spawn writer(app.holder, 10)
                             ^
  fix: move 'name' into a `guard` block, stop reassigning it once the value is built, or hand over a copy built in the call
```

a lock taken by hand, with `lock()` and `unlock()` or a `compare_set` spin,
does not count, since the checker cannot see which fields it covers. put the
field in a `guard` block.

a closure captures each value it uses by handle, so the same rule applies to
what a closure captures when the closure can run on another task. the
checker follows every function value through the whole program: a closure
reaches another task when it is a spawn's argument or callee, a sent value, a
value a module global holds in a program that spawns, a field of a struct
that crosses to a task, a capture of another closure that does, or when it
flows into a parameter, a field, a local or a function result that reaches
one of those. such a closure may not capture a collection, or a struct that
reaches one outside a `guard` block or reaches a counted field some code
reassigns. a closure a spawned function returns was made on that task and
runs on the one that awaits it, so it is judged the same way. a closure made
in a generic function is judged by the types its captures take in each
specialization the program uses. a closure that stays on the task that made
it, such as one passed to `map`, may capture anything. the message names the
capture and the path to the task.

```
error[E248]: a closure that reaches another task cannot capture 'seen' (a list), which the scope that made the closure still holds; the closure reaches the task through parameter 'handler' of 'get', then parameter 'handler' of 'handle', then ... a spawn at std/web.pith:758
  9 |         seen.push(req.path)
              ^
  fix: build the value inside the closure, hand the task a copy, or keep it in a `guard` block
```

request handlers are the usual case: `std.web`, `std.net.http`,
`std.net.http2` and `std.net.grpc` run a handler on the task serving each
connection. keep state the handler changes in a `guard` block, and state it
only reads in a `frozen` field. a builder that sets a field after building the
struct unsettles the field too, so build the struct with every field in the
constructor call, as `std.net.http`'s `Client.copy_with` does.

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

### E251 — private item or internal builtin

a module function declared with a bare `fn` belongs to the module that declared
it; only a `pub fn` can be reached through an import (`mod.name` or
`mod.name(...)`). the two live in one shared name table, so before this check
the lookup for `mod.helper` would fall through to a bare-name match and return
something the caller had no business holding.

the same code refuses two other encapsulation violations:

- reading, assigning, or constructing a struct field declared without `pub`
  outside the module that declared the struct (`ByteArray(handle: 7)` or
  `a.handle`), so code cannot copy or forge a private resource handle;
- calling the raw-handle `byte_buffer_*` runtime builtins outside `std.bytes`,
  which owns `ByteBuffer` and `ByteArray`.

mark the function or field `pub` if it is meant to be part of the module's
surface, or call the public entry point that wraps it.

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

the same code reports a binding in a nested block that reuses a name an
enclosing block of the same function already bound: a `:=`, an `if let`,
`while let` or `match` payload, a select arm's received value, or the name of a
`catch err:` block. a function has one slot per name, so the inner binding
would overwrite the outer value instead of shadowing it. sibling blocks may
reuse a name, and a `for` variable may reuse a local's name, since it gets a
slot of its own for the length of the loop.

a closure is a function of its own, so its body may bind a name the enclosing
function uses. its parameters and the names it captures share its slots,
though, so a binding in a nested block of the body that reuses one of those
is reported too.

```
error[E262]: 'x' is already bound in this function; a nested binding overwrites it rather than shadowing it
error[E262]: 'x' is captured from the enclosing function; a nested binding in the closure overwrites the capture rather than shadowing it
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

```
error[E266]: 'items' names a module global that is read earlier in this function, and this binding takes the name for the whole function
 10 |     mut items: List[Int] := [7]
          ^
  fix: rename the binding, or move the reads of the global into a function that does not bind the name
```

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
the impl is written in. the destructor spells the method symbol from the
declaring module, and an enum, generic or not, releases tagged payloads rather
than fields. a generic struct is allowed: each instance gets its own destructor,
which calls the `drop` checked against that instance's type arguments. the impl
must declare `fn drop()` with no parameters and no return value, since the
runtime hands it the bare pointer and reads nothing back, and nothing else
beside it.

```
error[E268]: impl Drop for Shape: only a struct can implement Drop
error[E268]: impl Drop for Bag: Drop must be implemented in the module that declares the struct
error[E268]: impl Drop for Slot: drop() takes no parameters
error[E268]: impl Drop for Slot: the impl may only declare drop(), not 'close'
error[E268]: impl Drop for Slot: the impl must declare fn drop()
```

`Drop` is a language-level interface: nothing declares it and nothing imports
it. put the type's other methods in an `impl T:` block of their own, and have
`drop` call the explicit closer so the two stay idempotent.

### E269 — assignment to a captured variable

a closure captures the outer variables it uses by value, when it is created.
an assignment to one inside the closure would change only the closure's copy:
`mut n := 0; c := fn() -> Int: n = n + 1; return n` returned 1 on every call
and `n` never changed. the checker rejects the assignment. return the new
value from the closure and assign it outside, or keep the state in a struct
field or a collection, whose handle the closure shares with the caller. a
module global is not captured, and a closure can assign its own locals.

```
error[E269]: cannot assign to 'n' inside a closure: a closure captures a copy, so the variable outside would not change
  fix: return the new value from the closure, or keep the state in a struct field or a collection the closure can update
```

### E270 — missing return

a function that declares a return type reaches the end of its body on some
path without returning a value. that path used to return a zero: 0 for an
`Int`, `[]` for a list, `none` for an optional, a failed result with no
message for `T!`, and a null struct that crashed on its first field read.
there is no implicit `none` or ok, so `-> T?` and `-> T!` follow the same
rule.

a path returns when it ends in `return` or `fail`, in a call to `exit`, or in
a `while true` loop with no `break` for it. an `if` returns when it has an
`else` and every branch returns, and a `match` when every arm does. a `match`
that is the last statement of the body returns its value: an inline arm
(`Color.Red => "red"`) returns its expression, and a block arm has to return
on its own. a closure with a block body follows the same rule when it
declares a return type or returns a value on some path. `pith lint` reports
the same code as a warning.

```
error[E270]: 'mk' declares a return type, but a path through its body reaches the end without returning a value
  fix: end every path with return or fail
error[E270]: this closure returns a value, but a path through its body reaches the end without returning one
```

### E271 — guarded data without its lock

data declared in a `guard` block can only be read or written with its mutex
held: inside a `locked` block for it, or in a function marked `holding` it.
a read counts, since reading a map while another task inserts into it is as
unsafe as the write. a struct field under `guard pick:` needs `pick` held on
the same value, so `p.next` needs `p.pick`, and the value has to be reached
through a path the checker can compare: a variable, `self`, or a field read
off one. a call to a function marked `holding m` needs `m` held at the call;
a path rooted at `self` is read off the receiver, and one rooted at a
parameter off that argument. a closure body runs when it is called, so the
locks held where it is made do not count inside it. code outside a function
runs at module init, before any task, and needs no lock.

```
error[E271]: 'cache' is guarded by cache_mu, which is not held here
  fix: wrap the access in `locked cache_mu:`, or mark the function `holding cache_mu`
error[E271]: 'p.next' is guarded by p.pick, which is not held here
error[E271]: 'evict' must be called holding cache_mu, which is not held here
```

### E272 — a locked block's mutex

`locked m:` takes `m` on entry and releases it at every exit from the block,
which means evaluating `m` again at each exit. so `m` has to be a path that
names the same `Mutex` every time: a variable, `self`, a field read off one,
or a list element indexed by a variable or a literal. the body may not
assign the path, a part of it, or an index variable in it, and may not
rebind one of those names, since the unlock would then reach a different
mutex. a `locked` block outside a function is rejected the same way.

```
error[E272]: locked needs a Mutex, got Int
error[E272]: a locked block needs a variable, a field or an indexed element that holds a Mutex
  fix: bind the mutex to a name first: `m := pick_lock()`, then `locked m:`
error[E272]: the body of this locked block changes 'i', so the unlock at its exit could reach a different mutex
  fix: change 'i' outside the locked block
```

### E273 — locking a mutex the block already holds

a `Mutex` is not reentrant. a `locked` block nested inside one that already
holds the same mutex would park the task on itself for good, so the checker
rejects it. the check compares the two paths as written, inside one function
or closure body: it does not follow a call into a function that takes the
same lock.

```
error[E273]: 'm' is already held by an enclosing locked block, and locking it again would deadlock
  fix: remove the inner locked block; the outer one already holds the mutex
```

### E274 — a guard block's shape

a module guard block holds only `mut` globals, and none of them may be `pub`,
since another module could not take the mutex. a sharded guard,
`guard m[n]:`, needs a count of at least 1 and is only allowed on module
globals. a guard block is not allowed in a generic struct, and a field inside a
guard block cannot be `frozen` (a frozen field is immutable and needs no lock).

```
error[E274]: a guarded global cannot be pub, since another module could not take its mutex
error[E274]: a guard block holds only `mut` globals
error[E274]: a sharded guard is only allowed on module globals
error[E274]: a guard block is not allowed in a generic struct
error[E274]: field 'x' cannot be both frozen and inside a guard block
```

### E275 — a frozen field is changed

a field declared `frozen` is set when its struct is built and never changed
after (see docs/ownership.md, "frozen fields"). reassigning it, assigning to
one of its elements, or calling a method that changes it in place (`push`,
`insert`, `remove` and the like) through a path that runs through it is
refused. build a new struct with the changed value, or drop `frozen` and keep
the field in a `guard` block.

in a program that spawns tasks, a frozen field's value may not be changed
through another name either: a local the value was copied into, a parameter
it was passed as, or a value that holds it. nor may it be stored into a value
a caller handed in, or go where the checker cannot follow it, such as a
closure's argument.

```
error[E275]: 'rows' is frozen, and its value is changed in place through another name, while another task may be reading it
  13 |     xs.push("main")
           ^
  fix: change a copy, or move the field into a `guard` block
```

```
error[E275]: 'routes' is frozen, so it cannot be changed in place
  8 |     r.routes.push("/admin")
          ^
  fix: build a new value with the change, or drop `frozen` and keep the field in a guard block
```

### E276 — a guarded value leaves its lock

a guarded field or global holding a collection, or a struct that reaches one
outside a guard block, is reached safely only while its mutex is held. inside
`locked`, its value may not be stored in a variable declared outside the
block, returned from the block, or captured by a closure made in the block:
each keeps a handle to the buffer after the lock is released.

it also may not be stored into a container that outlives the block. that is
a list, map, set or struct reached through a local declared outside the
block, a parameter, `self` or a global: `out.push(items)`, `m.insert(k,
items)`, `box.f = items`, and the same with the value wrapped in a tuple, a
struct or an optional. a call is judged by the callee's summary, which the
sharing pass computes for each function over the whole program, so generic
functions and interface methods are followed. passing a guarded
value to a function that stores the parameter into another argument that
outlives the block, or lets the parameter escape (into a global or code the
checker cannot follow), is refused. so is passing it to a
function value or a method on a type the checker cannot name, which may keep
it. a call that only reads its argument keeps nothing and stays allowed:
`len`, iteration, indexing, and a function that reads the list and returns a
count.

```
error[E276]: this value holds the guarded global 'items', and 'keep' stores it in an argument that outlives the block, so the buffer is reached after the lock is released
  22 |         keep(out, ident(items))
                         ^
  fix: copy what is needed inside the block
```

a name bound inside the block to a guarded value is guarded too, for the rest
of the block: a local bound to it (`list := self.idle[key]`), a loop variable
over it, an `if let` or `while let` binding of it, the result of a builtin
accessor on it (`get`, `first`, `last`, `unwrap`, `unwrap_or`, `expect`,
`value`, `values`), and the result of a function that returns one of its
parameters when a guarded value is passed in. a value that holds a guarded
buffer some other way, such as a struct a call builds around it, may not
leave the block either. storing such a value back under the same guard
(`rooms[name] = room`) is not an escape. a copy (`list.slice(0,
list.len())`, or `copy_list` of a list whose elements hold no buffer) is a
new buffer and may leave.

a block that moves the value out and gives the field a new one, `out =
self.items` then `self.items = []`, is a take and is allowed, since the guard
no longer reaches the old buffer. so is reading an element and removing it
in the same block (`v = xs[k]` then `xs.remove(k)`), and `take` on a guarded
map. a value that never changes after it is built belongs in a `frozen`
field instead of a guard.

a value stored into a guarded collection is reached under the lock from then
on, so nothing outside the lock may still hold it. it must be fresh or a
moved local, as for a value handed to another task (see E248): built in the
call, or a local the scope does not use again after the store. a parameter
stored this way makes each call judge its argument the same way, so a pool's
`release(conn)` refuses a caller that keeps using `conn`. a guarded field set
when its struct is built, and a guarded global's initializer, are judged as
stores. a value read under the same guard may be stored back; a guard that
holds such a value may then hold one buffer twice, so a value taken out of it
is not counted as fresh.

```
error[E276]: 'cache' is guarded, and this stores a value that shares a buffer with 'v', which this scope uses again, so the buffer is reached outside the lock
  16 |         cache.push(v)
                          ^
  fix: store a copy, or build the value inside the block
```

the same applies to a guarded struct, optional, tuple or enum whose fields
reach a counted field that some code reassigns (see E248). a task that writes
that field under the lock frees the value the copy outside the block still
reads.

```
error[E276]: 'item' is guarded, and this stores its value where it outlives the block, and its field 'name' is reassigned at app.pith:13, so a write under the lock frees the value read after the block
  19 |         out = b.item
                       ^
  fix: copy what is needed inside the block, move 'name' into a `guard` block, or stop reassigning it once the value is built
```

```
error[E276]: 'messages' is guarded, and this returns its value out of the block, so the buffer is reached after the lock is released
  11 |         return inbox.messages
                      ^
  fix: copy what is needed inside the block, or declare the field `frozen` outside the guard block if it is never changed
```

---

## lint warnings and later checker errors (E3xx)

`E300`–`E308` and `E310`–`E312` are lint warnings reported by `pith lint`.
`pith lint` exits non-zero when any warning is reported, though warnings do not
block `pith build` or `pith run`. `E309` and `E313` to `E319` are hard checker
errors emitted during `pith check` / `build` / `run` / `test` (`E309` is also
reported by `pith lint`).

### E300 — snake_case required (warning)

function names, variable names, and method names must use `snake_case`.

```
warning[E300]: function name 'GetUser' should be snake_case
  1 | fn GetUser():
       ^^^^^^^
```

### E301 — PascalCase required (warning)

type names (structs, enums, interfaces, type aliases) must use `PascalCase`.

```
warning[E301]: struct name 'my_point' should be PascalCase
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

### E306 — strong reference cycle (warning)

a struct holds a strong reference back to itself, directly (`next: Node?`) or
through a chain of fields across other structs in the same module. a cycle of
strong references keeps its own count above zero, so it is never reclaimed;
mark the back edge `weak` (see [ownership.md](ownership.md)). a `weak` field
is not an edge, and neither is a function-typed field. a cycle that runs
through another module's structs is not visible to the linter and passes.

the warning is given at a field that names a struct directly or as an
optional, since only such a field can be `weak`. a cycle made only of
collection fields, such as a tree's `children: List[Entry]`, is not reported:
a collection cannot be weak, and a tree closes a loop only if a node is put
inside its own descendants. a parent pointer back up such a tree
(`parent: Entry?`) is reported.

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

the rule does not fire for a value whose type implements `Drop`
(docs/ownership.md, "destructors"): its destructor gives the resource back when
the last value naming it goes away, so a value left to the end of its scope is
not a leak. the `std.net.tls` config builders are on the constructor list and
stay quiet for that reason. the parser scopes (`json.open_scope()`,
`toml.open_scope()`, `yaml.open_scope()`) have no destructor: a scope opened and
never closed holds every node parsed after it, so the rule reports one, with
`defer json.close_scope(scope)` as the fix. a std type that later implements
`Drop` goes quiet without a change to the list.

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
closing it, and a type with a destructor needs no entry at all. the survey
behind the list is in [destructors_roadmap.md](destructors_roadmap.md).

the message names the constructor and the closer to defer:

```
warning[E307]: 'scope' is built by json.open_scope() and never closed; the resource behind the handle is not reclaimed when the binding goes out of scope
  4 |     scope := json.open_scope()
                                   ^
  fix: close it on every exit -- `defer json.close_scope(scope)` next to the build; see docs/destructors_roadmap.md
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

### E309 — an unguarded shared global in a program that spawns

a `mut` module global that holds a list, a map, a set or a struct is one buffer
every task can reach, and two tasks writing it at once corrupt it. a `mut`
global of another counted type (a string, bytes, an optional, a tuple, a
function value, an enum with a payload) is one pointer, and a write releases
the old value while another task may be reading it. in a program that spawns
tasks, `pith check`, `build`, `run` and `test` refuse each such global that is
written after module init and is neither declared in a `guard` block nor
`threadlocal`, and `pith lint` reports it as well. the error names one spawn
the program reaches. a `Mutex`, `AtomicInt`, `WaitGroup`, `Semaphore`,
`Channel` or `Task` global synchronizes itself, so for one of those only an
assignment of a new handle counts as a write.

a global, `mut` or not, is also refused when its value reaches a counted field
that some code reassigns (see E248): every task can reach the global, and the
write frees the value another task reads.

a global holding a list, a map, a set or a struct that reaches one, `mut` or
not, may not be changed in place through any name either: not directly
(`items.push(x)` on a global that is not `mut`), not through a local it was
copied into, not through a handle a function or a task returns, and not by a
function it is passed to. nor may its buffer be stored into a value a caller
handed in, or go where the checker cannot follow it. a global only read needs
no lock.

```
error[E309]: 'names' is a module global every task can reach, and its buffer is changed in place at app.pith:19, in a program that spawns tasks
  6 | names: List[String] := []
                             ^
  fix: declare it in a `guard` block, mark it `threadlocal`, or change a copy
```

the rule covers the whole program the file loads, so a library global is
reported when a program that spawns uses it. a program spawns when a function
in the entry file, or the top level of any module it loads (a global's
initializer, a field default), holds a `spawn` or reaches a function that
does. code reaches a function by calling it or by naming it without a call:
`f := lib.launch`, a struct field or a global holding it, an argument passing
it. a named function may be called wherever the value goes, so it counts as
reached where it is named. a call is resolved to its module's function, its
import or, for a method, its receiver's type; a method on a receiver whose
type does not say whose method it is, such as an interface, counts every
method of that name. a library that spawns in a function the program never
calls or names does not make the program spawn. a write is an assignment
rooted at the global (`g = x`,
`g[k] = x`, `g.f = x`) or a call of a mutating method on it (`push`, `insert`,
`remove`, `clear` and the like), through the global's name or any other.

a global that is built once and only read afterwards needs no lock. build it in
its own initializer, `catalog := seeded()`, rather than in a function called
from `main`, and drop the `mut`.

```
error[E309]: 'hits' is a mut module global holding a shared buffer, written after module init, in a program that spawns tasks (app.pith:7)
  1 | mut hits: Map[String, Int] := {}
                                     ^
  fix: declare it in a `guard` block, mark it `threadlocal`, or stop writing it after init
```

```
error[E309]: 'current' is a mut module global holding a counted value, which a write releases while another task may be reading it, written after module init, in a program that spawns tasks (app.pith:9)
  1 | mut current: String := "start"
                             ^
  fix: declare it in a `guard` block, mark it `threadlocal`, or stop writing it after init
```

### E310 — a call that parks while a mutex is held (warning)

a task that parks inside a `locked` block, or anywhere in a `holding`
function, keeps the mutex for as long as it is parked, and every task that
needs the mutex waits for the park too. `pith lint` reports these calls when
the mutex is held:

- `await`
- a `select` with no `default` arm
- `sleep` and `time.delay`
- a channel `recv`
- `fs.read` and the other `std.fs` reads
- `tcp.accept`, `tcp.connect` and the `std.net.tcp` reads
- the reads, receives and accepts of `tls.Conn`, `tls.Listener` and
  `websocket.Conn`

a channel `send` is not reported. it parks only when the channel is full, and
sending under the lock is how a writer keeps its frames in order. a closure or
a `spawn` inside the block runs later or on another task, so neither is
checked, and a call to a function that parks somewhere inside it is not
followed.

```
warning[E310]: a channel `recv` runs with a mutex held, so the task parks holding it and every task that needs the mutex waits for the park too
  7 |         job := jobs.recv()
                     ^
  fix: park before taking the lock or after releasing it, and copy what the call needs while the lock is held
```

### E311 — two mutexes taken in opposite orders (warning)

a task that holds mutex A and waits for B, while another task holds B and
waits for A, deadlocks both. `pith lint` records every place in the program a
mutex is taken while another is held and reports a pair taken in both orders.
a mutex taken inside a function called while another is held counts, through
calls the checker resolved; a call through a function value is not followed,
and a closure or a `spawn` inside the block is not walked.

a mutex is named by where it lives: a module global by its module and name, a
struct field by the struct's type and the field, and every element of a
sharded guard by the guard's name. two values of one struct share a name, so
nesting them, as a transfer between two accounts does, is not reported, and a
mutex reached through a local or a parameter of type `Mutex` is skipped.

```
warning[E311]: 'ledger.b_mu' is taken here while 'ledger.a_mu' is held, and ledger.pith:16 takes 'ledger.a_mu' while 'ledger.b_mu' is held; two tasks taking these in opposite orders can deadlock
  10 |         locked b_mu:
                      ^
  fix: take the mutexes in one fixed order everywhere, or release the first before taking the second
```

### E312 — a call that takes a mutex already held (warning)

a mutex is not reentrant, so a task that holds one and calls a function that
takes it again waits for itself forever. E273 refuses a `locked` block nested
in a block on the same path; `pith lint` reports the same mistake made
through a call. a module global is matched through the callee and everything
it calls, except a sharded guard, whose elements share one name. a struct's mutex is matched when a method called on the receiver
whose mutex is held locks that field on `self`, so `self.encode()` inside
`locked self.mu:` is reported when `encode` takes `self.mu`, while a method
called on a different value of the same struct is not.

```
warning[E312]: this call takes 'self.mu', which is already held here; a mutex is not reentrant, so the task deadlocks
  21 |             self.bump()
                   ^
  fix: call a version that expects the lock held, marked `holding`, or release the mutex before the call
```

### E313: assignment into an index of a String or Bytes

a `String` and a `Bytes` value are immutable, so `s[i] = v` and `b[i] += 1`
are refused. build a new value instead, or keep bytes that change in a
`ByteBuffer`.

```
error[E313]: cannot assign into an index of a Bytes: the value is immutable
  3 |     b[0] = 70
             ^
  fix: build a new value, or use a ByteBuffer for bytes that change
```

### E314: an argument label that does not match its parameter

arguments bind by position. a label on an argument (`scale(by: 2)`) is
checked against the name of the parameter in that position, so a label that
names a different parameter, or none, is refused rather than bound to the
wrong one. a call through a function value takes no labels, since the value
carries no parameter names.

```
error[E314]: argument label 'b' is in the position of parameter 'a'; arguments bind by position
  5 |     print("{scaled(b: 1, a: 5)}")
                         ^
```

### E315: retired

E315 refused every interface used as a value type. Interfaces became value
types (docs/interface_values.md), and the interfaces that still cannot be
values are reported as E316.

### E316: an interface that cannot be a value type

an interface used as a type holds any value whose type implements it, and a
call through it is dispatched at run time. the call has to know the method's
whole signature without knowing the concrete type, so an interface with type
parameters, with an associated type, or with a method that has type
parameters of its own cannot be a value type. it can still bound a type
parameter.

```
error[E316]: interface 'Mapper' cannot be used as a value type: its method 'apply' has type parameters
  8 |     m: Mapper
           ^
  fix: take a type parameter bounded by it, as in `fn f[T: Mapper](x: T)`
```

### E317: a parameter or field default that is not allowed

a call may leave out a trailing parameter that declares a default
(`fn scaled(x: Int, by: Int = 10)`, called as `scaled(3)`), and the default
is passed in its place. the default is evaluated at each such call, so it has
to mean the same thing everywhere: a literal, a negated number, `none` for an
optional parameter, or an enum variant without a payload. arguments bind by
position, so a parameter without a default cannot follow one that has one.

```
error[E317]: the default of parameter 'b' must be a literal, `none` or an enum variant without a payload
  10 | fn computed(a: Int, b: Int = base()) -> Int:
                                    ^
  fix: pass the value at the call, or compute it in the body from an optional parameter
```

a generic struct's field default is one expression that every instance
shares. when the field's type names a type parameter, each instance builds
the default from its own field type, which works for an empty `[]` or `{}`
into a container field and `none` into an optional one. any other default of
such a field, such as a constructor call or a literal with elements, would
carry one type into every instance, so it is refused.

```
error[E317]: the default of field 'bag' names a type parameter, so it must be an empty `[]` or `{}` for a container or `none` for an optional
  12 |     bag: Bag[T] = Bag()
                         ^
  fix: pass the value when constructing, or build it in a function that returns the struct
```

### E318: a drop method that lets self escape

the runtime frees a struct that implements `Drop` as soon as its `drop`
method returns, so a reference to `self` that outlives the call would point at
freed memory. a `drop` body may use `self` to read and write its fields, as the
receiver of a method call, and as a call argument. a method or function that
`self` is lent to is held to the same rule for its receiver or that parameter,
through every call it makes, and a local bound straight from `self`
(`me := self`) counts as `self`. everything else is refused: storing it in a
field, a global, a container or a channel, returning it, building a struct,
list or optional around it, capturing it in a closure or a `spawn`, and passing
it to a builtin or anything else whose body the checker cannot follow. the
message names the chain of calls down to the use that lets it escape. the
field values `drop` copies out are ordinary values and may go anywhere (see
docs/ownership.md, "destructors").

the `drop` of a generic struct is checked the same way. its body has concrete
types only per instance, so the checker walks it against every instance the
program builds and then applies this rule once, after the first instance. a
generic struct that no code ever builds has no destructor and is not checked.

```
error[E318]: `self` escapes drop: it is passed to 'remember', where it is passed to 'store', where at line 20 it is passed to 'push', whose body the checker cannot follow; the value is freed when drop returns
  26 |         self.remember()
               ^
  fix: use self only to read and write its fields and to call methods that do the same; copy out the field values that must outlive it
```

### E319: an integer literal outside its type's range

an integer literal has to name a value of its type, in any base and with any
`_` grouping. in an expression that type is `Int`. in a `match` arm it is the
type of the value being matched, so a `UInt` pattern reaches
`18446744073709551615` and a `UInt8` pattern stops at `255`. without this
check a literal past its range passed `pith check` and then failed the build
in the backend.

a minus sign written straight before a literal is checked with it, so
`-9223372036854775808`, the smallest `Int`, is allowed and
`9223372036854775808` alone is not. `0 - 9223372036854775808` and
`-(9223372036854775808)` are refused, because there the literal stands alone.

the width and precision of a format spec (`{x:8.2}`) and the count of a
sharded guard (`guard m[8]:`) are numbers in the source too, and each has to
fit in an `Int`. a format spec that large is usually a brace meant to be
printed: `"{\"qty\":5}"` interpolates `"qty"` padded to a width of 5, and
`"{{\"qty\":5}}"` prints the braces.

```
error[E319]: the integer literal 18446744073709551616 is outside the range of Int, -9223372036854775808 to 9223372036854775807
  10 |     big := 18446744073709551616
                  ^
  fix: a whole number past 64 bits needs std.bigint
```
