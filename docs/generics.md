# generics: how a generic body is checked

A generic function, a generic method on a concrete type, a method of a
generic struct, and a generic method of a generic struct all have bodies with
no concrete types of their own. `T` is a name until something instantiates the
declaration. This page describes when those bodies are type-checked, what the
check knows, and how to read what it reports.

## the two passes

Pass 2 of the checker (`c_check_all` in `self-host/checker.pith`) walks every
concrete body with every top-level name in scope. It skips a generic body: with
`T` unresolved there is nothing to check a `T`-typed expression against, and
an error reported there would name a type the program may never use.

The generic-body pass runs after it, once per module, over a work queue:

- every generic call the concrete code makes records the type arguments the
  checker inferred for it (or resolved from an explicit `f[Int](x)` or
  `x.describe[Int](v)` spelling) and queues the pair (declaration, type
  arguments);
- every method call whose receiver is a generic struct instance
  (`holder.size()` on a `Holder[String]`, or the `next` a `for` loop calls on
  an iterator instance) queues that method against that instance. A method
  the program never calls at some instance is not walked there: `size()`
  reading `self.item.len()` is fine on a `Holder[List[Int]]` and would be a
  fault on a `Holder[Int?]` the program only ever builds;
- a method with type parameters of its own on a generic struct
  (`impl Box[T]: fn map[U](f: fn(T) -> U) -> Box[U]`) queues the triple
  (declaration, receiver instance, method type arguments): `Box[Int].map` at
  `String` and `Box[String].map` at `String` are two entries, and each is
  walked with the owner's parameters resolving to the instance's arguments
  and the method's to the call's;
- a queued body is walked with its type parameters resolving to the concrete
  types, `self` bound to the instance where there is one, and the associated
  types of the instance's impl in force. A generic call met inside the body
  queues in turn, so the pass reaches every specialization the program can
  run.

Each (declaration, types) pair is walked once per build. A body a program
never instantiates is still never checked.

## what a report looks like

A fault inside a generic body is reported at the line it sits on, with the
specialization it was found at appended to the message:

```
error[E209]: no method 'nonexistent' on 'Int' in first[Int]
```

The same fault at the same node reports once, named after the first type set
that reached it. A body that is wrong at one type and right at another (`t + 1`
called with `String` and with `Int`) reports for the type that fails, which is
what the suffix is for. A generic method of a generic struct names both lists,
`in Box[Int].map[String]`.

The check does not verify that a body uses only what its bounds promise. A body
that compiles at every type the program instantiates it at is accepted, even if
another type satisfying the same bound would not compile. That is a known
limitation, not a guarantee.

## the mode switch

`PITH_CHECK_GENERIC_BODIES` selects what the pass does. It is read once per
check run.

| value | effect |
|---|---|
| `off` | the pass does not run; generic bodies are left as pass 2 leaves them |
| `silent` | the pass runs and records nothing; the emitter's type reads still benefit |
| `count` | faults are recorded as warnings, so `pith check` prints them and exits 0 |
| `on` | faults are recorded as errors |

The default is `on`: a fault inside a generic body is an error. The pass ran
silently for a while so the corpus could be surveyed for faults inside bodies
that nothing had ever checked; the survey's findings were settled and it reads
zero, so the reports are on. `tooling/generic_body_dryrun.sh` still runs the
checker in `count` mode over `tests/cases`, `examples`, `std` and `self-host`
and tallies what it finds by code and by file, which is how a large body of
new code is surveyed before its faults become errors. `silent` keeps the walk
(the emitter still needs it) and drops the reports for one run.

## what the emitter reads

The expression types the pass computes are one specialization's answers for
the body's nodes. The pass keeps them out of the cache pass 2 filled, so a
`c_get_expr_type` on a node inside a generic body still answers the error type
after the pass. When the emitter is about to emit a specialization it asks the
checker for the same walk again, silently (`c_check_specialization` for a
generic function or a generic method on a concrete owner,
`c_check_method_specialization` for a method of a generic struct instance),
and reads the types for that body straight after. The type arguments the
checker recorded at the call that first asked for the specialization
(`c_generic_call_subst`) name the walk; the emitter copies them into its
specialization record when it queues the body, since the call's own entry is
overwritten by later checks of the same node.

A specialization is keyed by those recorded type arguments, and its symbol is
spelled from them: a primitive, struct or enum spells as its emission kind
(`first__int`, `nothing__CounterMsg`), a composite spells its structure
(`first__opt_int`, `first__opt_string`, `walk__list_int`), so two type-argument
lists that share an emission kind get two bodies. Two type ids with the same
spelling denote the same type, which is what dedupes them, since optional and
tuple types are not interned. The record exists for every generic call the
checker resolves, including a bare call to a generic imported by name and the
builtin spelling of `assert_eq`; the emitter has no type inference of its own.
A request that arrives with no call node (a json or config decode target) is
still keyed by emission kinds, and a body requested both ways is emitted once.

For a generic method of a generic struct the record is the whole key: the
receiver instance's type arguments first, then the method's own. The symbol
keeps the two lists apart by putting them where each already lives — the
instance arguments between the owner and the method name, as the per-instance
copies of the struct's plain methods spell them (`Box_int_show`), and the
method's own after the method name as every specialization does:
`Box_int_map__string` is `Box[Int].map[String]`, and `Box_string_map__string`
is a different body. The emitter splits the record at the owner's parameter
count, finds the instance the leading arguments name, and emits the body in
that instance's context with both lists substituting.

## writing the type arguments

A generic function's type arguments can be written at the call, `f[Int](x)`,
and so can a generic method's, `x.describe[Int](v)` or `x.blank[Int]()`. The
written list is resolved against the method's own bracket list in order, must
match its length (E221), and is checked against the bounds the way an inferred
list is; it is the only way to fix a parameter that appears in no argument,
which a bare call refuses (E222).

The parser decides what the bracket after `.name` means with one rule, from
tokens alone: it is a type-argument list when the token after its closing `]`
is `(`, everything inside it is a token a type can be spelled from, its first
word is a type name (an identifier beginning with an uppercase letter, or `fn`,
with `(` allowed ahead of one for a grouped or tuple type), and the receiver
is not one of the file's import aliases. Everything else keeps its meaning: an
index into a field of closures followed by a call, `t.items[0](y)` or
`t.items[i](y)`, holds a literal or a lowercase binding and stays an index; a
module function called with type arguments, `json.decode[Row](s)`, has an
import alias as its receiver and stays the shape the module paths read. The
spellings the rule misreads are an index that is itself type-shaped — a
SCREAMING_CASE constant or an enum variant indexing a field of closures,
`t.items[MAX](y)`, `t.handlers[Kind.Click](ev)`; the checker names the repair
(E209), which is to group the index, `(t.items[MAX])(y)`, or bind the element
first.

## a generic that never stops

A generic that calls itself at a larger type (`grow(x)` calling `grow([x])`)
asks for `T`, then `List[T]`, then `List[List[T]]`. The queue refuses a
declaration after 64 distinct type sets and reports E267 once.
