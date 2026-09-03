# generics: how a generic body is checked

A generic function, a generic method on a concrete type, and a method of a
generic struct all have bodies with no concrete types of their own. `T` is a
name until something instantiates the declaration. This page describes when
those bodies are type-checked, what the check knows, and how to read what it
reports.

## the two passes

Pass 2 of the checker (`c_check_all` in `self-host/checker.pith`) walks every
concrete body with every top-level name in scope. It skips a generic body: with
`T` unresolved there is nothing to check a `T`-typed expression against, and
an error reported there would name a type the program may never use.

The generic-body pass runs after it, once per module, over a work queue:

- every generic call the concrete code makes records the type arguments the
  checker inferred for it (or resolved from an explicit `f[Int](x)` spelling)
  and queues the pair (declaration, type arguments);
- every method call whose receiver is a generic struct instance
  (`holder.size()` on a `Holder[String]`, or the `next` a `for` loop calls on
  an iterator instance) queues that method against that instance. A method
  the program never calls at some instance is not walked there: `size()`
  reading `self.item.len()` is fine on a `Holder[List[Int]]` and would be a
  fault on a `Holder[Int?]` the program only ever builds;
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
what the suffix is for.

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

The default is `silent`: the pass runs in every build, so a generic struct
instance first made inside a generic body is registered before any emission
starts and the emitter finds the call-site type arguments recorded, but
nothing is reported while the corpus is being surveyed for faults inside
bodies that were never checked before. `tooling/generic_body_dryrun.sh` runs
the checker in `count` mode over `tests/cases`, `examples`, `std` and
`self-host` and tallies what it finds by code and by file; each finding is
either a real fault to fix or a check that assumed a concrete declaration site
and needs adjusting before the default moves to `on`.

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

Two specializations can share one emitted body: specializations are keyed by
emission kind, and every optional is the "tuple" kind, so `first[Int?]` and
`first[String?]` land on one key and the body is checked and emitted for the
first caller's types. Keying specializations on type ids is the follow-up that
separates them.

`PITH_SPEC_RECHECK=0` turns the emitter's re-check off for one build, which
makes every generic body read the error type again exactly as before: one
binary, two emissions, for measurement and for bisecting a difference.

## a generic that never stops

A generic that calls itself at a larger type (`grow(x)` calling `grow([x])`)
asks for `T`, then `List[T]`, then `List[List[T]]`. The queue refuses a
declaration after 64 distinct type sets and reports E267 once.
