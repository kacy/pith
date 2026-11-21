# interface values

Status: implemented. The plan at the end records what is left.

An interface used to be only a generic bound. `fn area_of[T: Shape](s: T)` is
specialized once per concrete `T`, and a value whose type was the interface
itself was refused with E315. A list of mixed shapes could not be written:
there was no `List[Shape]`, and the workarounds were an enum that wraps every
implementing type, or a struct of closures written by hand.

An interface value is a value of type `Shape` that holds any type implementing
`Shape`, with its methods dispatched at run time.

The interface name alone is the type. Rust's `dyn Shape`, Swift's
`any Shape`, a sigil and a `Box[Shape]` wrapper were considered and dropped:
the bracketed bound `[T: Shape]` already marks the specialized form, so a bare
`Shape` can only mean the dispatched one, and a keyword would be the only one
in Pith's type syntax (`dyn Shape?` reads ambiguously).

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
- an interface with type parameters (`Source[T]`), for the same reason;
- an interface with associated types: the caller cannot name them.

Pith has no `Self` type, so the rule Rust has for it does not arise.

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
`funcref` store per method. The box gets the tuple destructor for its field
signature (`0u000` for a struct payload and three methods), which releases
field 1 by its kind. Tuples already share these destructors per signature, so
every interface value with a payload of the same kind and the same method
count shares one, and the release path frees the value without knowing the
concrete type.

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
calli   r3 r2 int 1 r1           # the one new instruction
```

`calli REG PTR RETKIND NARGS ARG...` lowers to a Cranelift `call_indirect`
with N i64 parameters and one i64 return. That is the plain abi every function
already uses, and methods of an `impl Iface for T` block are never moved to the
register-result abi, so a result-returning method needs no second form.

Each slot points at the concrete method itself. No thunks are needed: a scalar
receiver (`impl Shape for Int`) is a word like a handle, and a default method
the impl did not override is emitted for the type under the impl's own method
name. The receiver is pinned (retained) for the call, as a closure field is,
and owned arguments are released after it the way a direct method call
releases them.

The checker records each conversion against the value's expression node, the
same way it records an optional widening, and the emitter builds the box
wherever it emits a node that carries a record. A collection literal stored
into a `List[Shape]` or `Map[K, Shape]` takes the target's type rather than
unifying its elements, which would refuse `[Sq(..), Label(..)]`.

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
- Handing a held interface value to another task is checked against every
  type that implements the interface (E248): the checker cannot know which one
  the value holds, so it refuses the value when any of them reaches an
  unguarded collection.
- An interface value satisfies its own bound: `f[T: Shape](x)` with `x: Shape`
  specializes at `Shape`, and the calls in the body dispatch.

## errors and the checker

- E315 goes away for interfaces that can be values, and is replaced by E316
  for those that cannot.
- Calling a method the interface does not declare on an interface value is
  E209, naming the interface.
- A conversion where the concrete type does not implement the interface is a
  type mismatch (E219) at the value.
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

Done:

1. The checker accepts interface types as value types, reports E316, records
   each conversion, and types method calls on interface values.
2. The IR has `calli`, in the consumer, the count checker and the contract.
3. The emitter builds the box at each recorded conversion and dispatches
   calls through the slots.
4. tests/cases/test_interface_values.pith covers every conversion site,
   test_interface_values_more.pith stores, closures, Float and fallible
   methods, the bound and a spawn, test_interface_values_cross_module.pith an
   imported interface, the
   tests/invalid cases each E316 reason and the mismatches, and
   tests/leaks/leak_interface_values.pith the counts.

Left:

- A default method on a primitive impl that calls another method on `self`
  does not check, because a method call on a primitive receiver resolves
  builtins only (docs/limitations.md). Through an interface value the
  primitive's own methods dispatch correctly.
- A generic struct cannot implement an interface (`impl Shape for Box[T]` is
  not matched by a bound either), so it cannot be converted.
- A bench case for a dispatched call against a specialized one, as a baseline
  for a shared table or devirtualization.
- Downcasts, embedding and value equality, as above.
