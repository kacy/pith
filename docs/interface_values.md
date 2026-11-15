# interface values: design

Status: proposed. Nothing here is implemented yet.

Today an interface is only a generic bound. `fn area_of[T: Shape](s: T)` is
specialized once per concrete `T`, and a value whose type is the interface
itself is refused with E315. A list of mixed shapes cannot be written: there is
no `List[Shape]`, and the usual workarounds are an enum that wraps every
implementing type, or a struct of closures written by hand.

This document proposes interface values: a value of type `Shape` that holds any
type implementing `Shape`, with its methods dispatched at run time.

## what a program can write

```pith
interface Shape:
    fn area() -> Float
    fn name() -> String

struct Circle:
    r: Float

impl Shape for Circle:
    fn area() -> Float:
        return 3.14159 * self.r * self.r
    fn name() -> String:
        return "circle"

struct Square:
    side: Float

impl Shape for Square:
    fn area() -> Float:
        return self.side * self.side
    fn name() -> String:
        return "square"

fn total(shapes: List[Shape]) -> Float:
    mut sum := 0.0
    for s in shapes:
        sum = sum + s.area()
    return sum

fn main():
    shapes: List[Shape] := [Circle(r: 1.0), Square(side: 2.0)]
    print(total(shapes).to_string())
```

The rules:

- An interface name is a type wherever a type is accepted: a parameter, a
  local, a struct field, a return type, a list, map value, set element,
  optional or result payload. There is no `dyn` keyword. A bound
  (`T: Shape`) keeps its current meaning and is still specialized.
- A concrete value becomes an interface value where the expected type is the
  interface: an assignment, an argument, a return, a field initializer, or an
  element of a literal whose type is known. The checker inserts the
  conversion. There is no conversion in the other direction yet (see
  "downcasts" below).
- A method call on an interface value is dispatched to the concrete type's
  implementation, including a default method the impl did not override.
- Two interface values compare with `==` by identity, as structs do now.

## which interfaces can be values

An interface can be used as a value type only if every method can be called
without knowing the concrete type. The checker reports a new error, E316, at
the use of the interface as a type, naming the method that prevents it:

- a method with type parameters of its own (`fn map[U](...)`): the
  specialization to call is not known at the call site;
- a method that takes or returns `Self`: the caller cannot name the type;
- an interface with associated types: the caller cannot name them either.

A bound use of such an interface is still fine. This matches what Rust calls
object safety, without the opt-out annotations.

## representation

An interface value is an ordinary counted struct handle. Its fields are:

| field | holds |
|---|---|
| 0 | a type id for the concrete type, a small integer assigned per program |
| 1 | the concrete value: a handle, or an Int, Float or Bool stored in the word |
| 2.. | one function pointer per interface method, in declaration order |

Building one is a `pith_struct_alloc(2 + methods)`, a constant store for the
type id, the value store (with a retain when the value is counted), and a
`funcref` store per method. The struct's destructor is a generated function per
(interface, concrete type) pair that releases field 1 with the concrete type's
kind, so the existing release path frees the value correctly without knowing
the type.

Why this layout:

- It needs no new value kind. Everything that already moves a struct handle
  (lists, maps, optionals, results, channels, the count checker, the leak and
  memcheck suites) moves an interface value unchanged.
- It needs no static data section. The table is copied into each value, which
  costs 8 bytes per method per value. Interfaces in the corpus have 1 to 4
  methods, so a value is 32 to 56 bytes plus the header. A shared table is a
  later optimization if the size matters.
- The type id makes downcasts possible later without changing the layout.

The alternative, a two-word fat pointer (value plus table pointer) as in Rust
and Go, avoids the allocation. It would need a two-register value kind
through the IR, the register-result ABI, every collection and the count
checker. The allocation is the smaller cost, and the struct pool already makes
small allocations cheap.

## dispatch

A method call `s.area()` on an interface value loads the function pointer from
the method's slot and calls it indirectly with field 1 as the receiver:

```
field   r1 s 8 int value         # the concrete value
field   r2 s 16 int slot         # area's function pointer
calli   r3 r2 float 1 r1         # new instruction
```

`calli REG PTR RETKIND NARGS ARG...` is the one new IR instruction. It lowers
to a Cranelift `call_indirect` with a signature built from `RETKIND` and the
argument kinds, the way `emit_closure_call` builds one for closures today. A
method that returns a result goes through a matching `rcalli`.

The target of each slot is the concrete method itself when its receiver is
already a word the slot can pass. A scalar receiver (an `impl Shape for Int`)
gets a thunk that takes the word and calls the method. A default method gets
the specialization the emitter already produces for that type.

## ownership

- Converting a borrowed value to an interface value retains it. Converting a
  value the expression owns moves it into field 1 without a retain.
- A method call borrows field 1 for the call, as a method call on the concrete
  value borrows `self`.
- The destructor releases field 1 when the interface value's count reaches
  zero. A weak reference to an interface value is a weak reference to the
  wrapper, not to the concrete value.
- A cycle through an interface value is a cycle through a struct and is
  handled the same way.

## errors and the checker

- E315 goes away for interfaces that can be values, and is replaced by E316
  for those that cannot.
- Calling a method the interface does not declare on an interface value is
  E209, naming the interface.
- A conversion where the concrete type does not implement the interface is
  the existing "does not implement" error, reported at the conversion.
- The generic body pass needs no change: a bound is still a bound.

## what is out of scope for the first version

- Downcasts (`if s is Circle`, or `match` on the concrete type). The type id
  is stored so this can be added without changing the layout.
- Interface values for interfaces from another module with generic methods
  satisfied by an impl in a third module. The first version requires the impl
  to be visible where the conversion happens, as generics do now.
- Embedding one interface in another.
- Value equality. `==` stays identity until structs get derived equality.

## plan

1. Checker: accept interface types as value types, add E316, insert the
   conversion node at the coercion sites, and type method calls on interface
   values. Tests for each coercion site and each E316 reason.
2. IR: the `calli` and `rcalli` instructions in the consumer, and the IR
   contract entry. A consumer unit test with a hand-written function pointer.
3. Emitter: the conversion (alloc, stores, destructor), the per-pair
   destructor, scalar thunks, and dispatch. Regression cases for a list of
   mixed types, a map of interface values, an interface value in a struct
   field, an optional and a result of one, and a method with a default.
4. Ownership: leak cases for every coercion site and for a value released
   through each container, then the count checker over the new cases.
5. Docs: a section in `docs/idiomatic_pith.md`, removal of the limitation from
   `docs/limitations.md`, and E316 in `docs/errors.md`.

Measure the cost of a dispatched call against a specialized one with a new
bench case, so later work on a shared table or devirtualization has a
baseline.
