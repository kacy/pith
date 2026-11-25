# The Pith IR contract

The Pith compiler is split in two. The self-hosted front end
(`self-host/*.pith`) type-checks a program and lowers it to a small text IR.
The Rust back end (`cranelift/codegen/src/ir_consumer.rs`) reads that text and
hands it to Cranelift for native code. This document is the contract between
them: the instruction set, the type vocabulary, the ABI conventions, and the
rules about what the consumer accepts and what it rejects.

It is meant to be authoritative. When the emitter and the consumer disagree,
one of them has a bug; this file says what the agreement is supposed to be.

## The shape of the IR

The IR is line-oriented text: one instruction per line, tokens separated by
single spaces, no punctuation. The consumer splits each line on whitespace and
dispatches on the first token. Blank lines are ignored.

There are two kinds of line: **declarations** that set up the module (strings,
functions, structs, globals) and **instructions** that make up a function body.
The consumer reads the whole file in two passes. The first declares every
function and piece of data so calls can resolve regardless of order, and the
second compiles each function body. When a `main` function is present, a
reachability pass runs before pass 1 so only live functions, live globals, live
cycle-collector tracer registrations, and deduplicated live string literals are
declared and compiled.

Values inside a body live in **registers**, written as bare integers (`1`, `2`,
`8`). A register is assigned once by the instruction that produces it and read
by later instructions. Registers are function-local. Named **variables**
(`store` / `load`) are the mutable slots that carry values across basic blocks.
A register can be read in a later block only when the block that defines it
runs on every path there: the inlined `contains` search, for one, keeps its
list and length in registers across its loop. A register defined on just one
branch is gone after the branches meet.

## Declarations

These appear at module scope. The consumer reads them in the first pass.

```
string N "content"          a string literal. N is its index; strref refers back to it.
func NAME NPARAM RETTYPE     opens a function. NPARAM is the parameter count.
param NAME [KIND]            names one parameter, in order (inside the func body).
                             KIND is `float` for a Float parameter and `word`
                             for one that is never a Float; without it the
                             consumer infers the kind from the body.
endfunc                      closes the current function.
struct NAME FIELD...         declares a struct's field layout.
struct_alias ALIAS TARGET    makes ALIAS resolve to TARGET's layout.
global NAME INIT_KIND ...    a module-level global and how to initialize it.
```

**A function is lowered as `nparam` i64 parameters returning one i64, except
under the register-result ABI**, where it returns two values (see ABI
conventions below). Apart from that case `RETTYPE` is descriptive, and the call's
retkind drives the real return handling for each call.

## Instructions

These make up a function body, between `func` and `endfunc`.

**Constants and string references**

```
iconst REG VALUE            integer constant.
fconst REG VALUE            float constant (stored bitcast into an i64 register).
strref REG STRIDX           load a pointer to string literal STRIDX.
```

**Integer arithmetic**

```
add|sub|mul|div|mod REG A B   REG = A op B. div and mod are checked (a zero
                              divisor traps through the runtime, not silently).
                              add, sub and mul wrap at 64 bits.
addc|subc|mulc REG A B        REG = A op B for source arithmetic on an Int: a
                              result outside the signed 64-bit range stops the
                              program with the integer-overflow runtime error.
uaddc|usubc|umulc REG A B     the same for UInt and UInt64, against the unsigned
                              range.
addnsw REG A B                REG = A + B on an Int, where the front end proved
                              the sum fits: a loop counter's step, or a counter
                              plus one before the step. it has no check, and
                              unlike `add` its result has not wrapped.
```

The consumer narrows a check when an operand is known never to be negative: a
constant of at least zero, a length, a load of a variable whose every store is
one of these, or the `addc`, `mulc` or `addnsw` of two of them
(`cranelift/codegen/src/nonneg.rs`). With such operands `a - b` needs no check,
`x * c` compares `x` with the top of its range only, and `a + b` compares the
sum with an operand instead of testing the overflow flag. `wrapping_add` lowers
to a plain `add`, so an `add` says nothing about the sign of its result, and a
sum the front end proved is written `addnsw`.

**Float arithmetic**

```
fadd|fsub|fmul|fdiv REG A B   float op on two float registers.
```

`add|sub|mul|div` (and the checked forms) also lower as float ops when either
operand is a float register. The consumer tracks which registers hold floats
and picks the instruction class accordingly. `add` on two string registers
lowers to `pith_concat_cstr`.

**Bitwise and logical**

```
band|bor|bxor|shl|shr REG A B    bitwise ops. a shl or shr count outside 0
                                 to 63 stops the program with the
                                 integer-overflow runtime error.
bnot REG A                       bitwise complement.
and|or REG A B                   lowered as bitwise band/bor (operands are 0/1).
```

**Comparison**

```
eq|neq|lt|gt|lte|gte REG A B     result is 0 or 1. dispatches on operand kind:
                                 integers compare directly, floats via FloatCC;
                                 ordered comparisons (`lt|gt|lte|gte`) dispatch
                                 strings via the pith_cstring_* runtime helpers
                                 (`eq`/`neq` compare raw words; string equality
                                 is emitted as an explicit `__str_eq` call).
```

```
udiv|urem REG A B                unsigned division and remainder, for UInt and
                                 UInt64; a zero divisor traps like `div`.
ult|ugt|ulte|ugte REG A B        unsigned ordering, for the same two types.
```

UInt and UInt64 hold their values as the bits of an i64, so the emitter uses
these for `/`, `%` and ordering on them; a value at or past 2^63 would read as
negative under the signed forms.

**Strings**

```
concat REG A B              REG = A followed by B, as a fresh heap string.
concatn REG COUNT PART...   REG = the COUNT parts joined, as one fresh string.
appendn REG COUNT BASE PART...   REG = BASE with the COUNT parts appended; the
                                 result takes over BASE's count.
```

The emitter writes only `concat`. `concatn` and `appendn` are made inside the
consumer, by `fuse_concat_chains`, `fuse_self_appends`, and `fuse_int_parts`,
from a chain of `concat`s (and adjacent `int_to_string` calls) whose
intermediate results nothing else reads.

**Optionals**

```
optflag REG OPT             1 when the optional OPT holds a value, 0 when it
                            is none.
optval REG OPT KIND NAME    the payload of OPT, typed by KIND, or 0 when OPT
                            is none.
```

The emitter reads optionals through these two forms only, never with `field`,
because a none has no shell to load from (see the optional encoding below).

**Calls**

```
call REG FNAME RETKIND NARGS ARG...
rcall FLAG_REG VAL_REG FNAME PAYLOAD_KIND NARGS ARG...
```

`call` covers everything that returns a single value: runtime functions, user
functions, struct construction, and void calls. `RETKIND` names how to treat the
result (see the type vocabulary). A void call is a `call` with a `void` retkind
whose result register is never read. There is no separate `callv`.

`rcall` is the register-result form. It calls a function that returns an
`(is_ok, payload)` pair and lands the two values in two registers: `FLAG_REG`
takes the ok flag, `VAL_REG` the payload, interpreted as `PAYLOAD_KIND`. The
callee is either a declared function whose retkind is `result_reg` or
`result_reg_f`, or a runtime builtin whose row in `runtime_functions.txt`
returns `I64,I64` (see the result encoding below). A `float` payload from a
runtime builtin arrives as the f64's bits in the second integer word, where a
`result_reg_f` function returns an actual f64; the consumer handles both. A
plain `call` to a pair-returning callee is a consumer error, as is an `rcall`
to anything else, and `ir_driver --validate` rejects both before the consumer
sees them.

**Memory and fields**

```
store VARNAME REG           write REG into the named variable slot.
load REG VARNAME            read the named variable into REG.
field REG OBJ OFFSET KIND NAME    read the field at byte OFFSET of OBJ. KIND is
                                  the field's type (drives result tracking);
                                  NAME is for readability only.
field REG OBJ INDEX               legacy short form: INDEX is a field index,
                                  offset = INDEX * 8, result kind unknown.
sstore STRUCT_REG FIELD_IDX VALUE_REG   store VALUE into field FIELD_IDX of a struct.
```

`VARNAME` is one flat namespace, and the consumer resolves it against the
module-level globals before the function's own variables, so a global and a
local of the same spelling would name the same storage. The emitter keeps them
apart by spelling: a local, a parameter and a pattern binding reach the IR
under the name they were written with, while a global is declared and reached
under `__g_<name>`. Nothing written in a source file produces that spelling, so
the two can no longer meet, and `ir_global_storage_symbol` is the single place
the mapping is applied.

**Function values**

```
funcref REG NAME            REG = a pointer to function NAME.
calli REG PTR RETKIND N ARG...
                            call the function PTR points at with N
                            arguments under the plain abi (i64 arguments,
                            one i64 return); RETKIND types the result the
                            way a call's does. a method call on an
                            interface value lowers to one, through the
                            method's slot (docs/interface_values.md). the
                            count checker treats it as a user call.
closure_ref REG NAME        REG = a closure value wrapping NAME.
closure_stack REG NAME      the same closure, built in a stack slot of the
                            current frame. never retained, released or
                            stored; the emitter uses it for a closure that
                            is only called directly and captures nothing
                            with a count, and for a lambda passed to a
                            parameter the callee only calls, which holds
                            its counted captures without a count.
tuple_stack REG N           REG = a struct of N fields in a stack slot of the
                            current frame, with the header pith_struct_alloc
                            writes. never released: the emitter uses it for
                            a result local only read in its own function,
                            whose cleanup drops the payload and leaves the
                            shell.
```

**Control flow**

```
ret REG                     return REG from the current function.
ret2 FLAG_REG VAL_REG       return both registers, under the register-result abi.
brif COND THEN ELSE         branch to label THEN if COND is nonzero, else ELSE.
jmp LABEL                   unconditional branch.
label NAME                  a branch target.
label NAME cold             a branch target on a rare path, such as an error
                            exit. the back end lays the block out after the
                            function's other blocks, together with the moves
                            on the branches into it.
```

## Count marks

With `PITH_CHECK_COUNTS=1` set, the emitter adds `mark` lines for the count
checker. They state facts the instructions leave out: a load or a `param`
doesn't say whether the value is an int or a string, and a local's exit
cleanup doesn't say which locals the emitter believes own a count.

```
mark global NAME KIND       a global's kind, after its `global` line.
mark kind NAME KIND         a local's or parameter's kind. seeded globals are
                            left out unless the function rebinds the name.
mark owns NAME KIND         the exit cleanup releases NAME, so the emitter
                            counts it as holding one count of KIND.
mark borrow NAME FROM       NAME holds no count of its own: it borrows from the
                            value in slot FROM, which must stay alive and
                            unchanged while NAME is used. a list loop's
                            variable borrows from the loop's iterable this way
                            when the body can't drop the element.
mark mixed NAME             NAME holds different kinds on different paths; a
                            side slot records which, and the cleanup reads it.
mark untracked NAME         NAME is never released by the cleanup.
```

A function's `kind`, `owns`, `mixed` and `untracked` marks come just before
its `endfunc`. A loop's marks come where the loop sets up its slots. A
slot with no mark is one the emitter made for a scalar, such as the flag a
short-circuit `and` keeps. An enum with no payload variant is marked `int`,
since it lowers to an integer tag. Functions the emitter makes itself, such as
destructors and enum equality, carry none.

A user function borrows every parameter: the caller keeps its count. A
parameter with an `owns` mark is one the function's prologue retains, and
its exit cleanup gives that count back. Runtime functions differ case by
case, and `cranelift/runtime-abi/runtime_ownership.txt` says how; `build.rs`
refuses a runtime function without a row there.

`PITH_CHECK_COUNTS=1 pith build file.pith` also runs the count checker
(`cranelift/codegen/src/count_check.rs`) on the marked IR before the
backend drops the marks. It prints, on stderr, how many functions balance,
how many don't, and how many it couldn't settle, with each unbalanced
function and the instruction that made the count in question. It checks three
rules:

- Balance: on every path, each count a function takes is given up exactly
  once, by a release, a store into a holder, or the return.
- Borrow: a loop variable marked `borrow` is not read after the loop body
  could have dropped what it borrows from, through an evicting method such
  as `clear` or `pop`, a store into the same storage, or a call into a
  function that writes it. A call the checker knows nothing about counts as
  a write.
- Untagged stores: a counted value is not stored into an untagged container
  (which cannot release it on teardown), and no struct is instantiated over an
  `<error>` placeholder type.

`PITH_CHECK_COUNTS=2` adds the join details and the names of the balanced
functions. `PITH_CHECK_COUNTS=only` reports and stops before code
generation, for tools that want only the report.

The build itself never fails on a finding. `make count-marks-check-only`,
which CI runs, checks every corpus program and example and fails on any
unbalanced function or borrow finding. A function the checker can't settle
is counted as undecided and doesn't fail the target. Over the corpus, 7490
distinct functions balance and 41 are undecided: 17 locals whose kind
changes by path, 7 slots read past a join that dropped them, 5 joins of
match or rebinding temps, 5 calls of a generic method on `T` (`to_string`,
`next`), 4 values of a kind no instruction settles and 3 JSON decoders' raw
word slots.

The checker models the register-pair ABI (`rcall`, `ret2`): the payload holds
one count, the ok value's or the error's, and a plain ok payload holds it
only when the flag is clear. It reads a result box's `ok` or `err` slot as a
payload the box hands over when it is freed, an optional's `optval` as a
field read, and a channel receive's flag as a null test that follows the
value into locals. A box passed to the emitted unshare keeps its payload,
and so does one released through an emitted release helper; a destructor
set to null leaves the box a shell. An `await` of a task returning `T!` is
typed `tuple`, so the box it hands back is counted like a call's.

The instructions themselves supply a few facts. A register the function
retains or releases by name holds a counted value, even where its kind is a
generic body's `T`, and so does one stored into an unmarked temp a counted
register is later loaded from. A call through a slot the function stores to,
such as the temp an open-coded `map` keeps its function in, is a closure call
under the user-function convention. A function the program defines wins over a
runtime function of the same name, as it does in the consumer. A weak upgrade
(`pith_struct_weak_upgrade`, `pith_struct_weak_load_field`) hands back a strong
count or a null, and a value a weak count was taken on
(`pith_struct_weak_retain`) is stored into a weak field or a weak capture
without a strong one; weak counts themselves are left out. An optional given a
destructor after its payload was read and retained (`m.get(k)` kept as an
optional) takes that count. Where two paths meet, an unmarked temp that holds a
count of its own is one more owner of the value it shares with a local. A count
given up that a path never held is reported even when a later join leaves the
function otherwise unsettled.

The checker was tested by mutation: delete one release, or duplicate one, in
a function it balances, and see whether it flags that function. A mutant
that releases a null (a local not yet assigned on that path) changes nothing
and is left out of the score; a retain put before the release tells which
those are, since the checker reports the extra count only on a value that
holds one. On `examples/api_gateway`, where 3522 functions balance, it caught
233 of 233 such mutants and 413 of 413 over two seeds; on
`tests/cases/test_process_command` it caught 298 of 298. Before the doubled
release that unsettles a later join was reported, the gateway scores were
248 of 249 and 426 of 427, and 296 of 298.

Marks generate no code. The consumer drops every `mark` line before its first
pass, so a mark can't split a pattern its rewrites match, and a build with
marks produces the same object code as one without. Without the variable, no
marks are written, and the dumped IR and the bootstrap seed are unchanged.

## The type vocabulary

The IR carries types as flat **kind strings**. The same vocabulary shows up as
a call's `RETKIND`, a `field` kind, and a global's init kind:

| Kind | Meaning |
|------|---------|
| `int`, `bool` | A plain integer register |
| `float` | A float, bitcast through an i64 register |
| `string` | A heap or literal C-string pointer |
| `bytes` | A byte buffer pointer |
| `list`, `list_string`, `map`, `map_int`, `set`, `set_int` | A collection handle (with specialized element/key flavors) |
| `channel`, `task` | A concurrency channel or task handle |
| `opaque:NAME` | A runtime opaque handle (such as `opaque:Mutex`, `opaque:WaitGroup`, `opaque:Semaphore`) |
| `struct` | A struct pointer of unspecified type |
| `struct:NAME` | A struct pointer known to be of type NAME |
| `tuple` | A tuple (a struct-shaped allocation) |
| `result`, `result_int`, `result_bool` | A fallible result (see encoding below) |
| `result_reg`, `result_reg_f` | A fallible result returned in registers: `(is_ok, payload)`, the payload i64 or f64 |
| `optional` | An optional value |
| `void` | No meaningful result |
| `unknown` | The emitter did not commit to a kind |

`unknown` is an explicit admission and does not act as a wildcard: the
consumer will not track the register's kind, and later kind-specific handling
(string release, float math, struct field access) will not fire for it. An
`unknown` where a concrete kind was knowable is a missed optimization or a
latent bug.

## ABI conventions

**i64 parameters, one or two returns.** Parameters are always i64: pointers are
i64, and floats are bitcast to i64 across call boundaries and cast back where
float math is needed. Returns are i64 too, except for the register-result ABI.
A function whose retkind is `result_reg` returns two values, `(is_ok: i64,
payload: i64)`; `result_reg_f` returns `(is_ok: i64, payload: f64)`. Everything
else returns a single i64. `push_return_types` in the consumer is the authority,
and `RETTYPE` does drive the machine signature for those two kinds. The runtime
builtins that return the same pair take their signature from their table row
instead (see the result encoding below).

**Who owns a returned pair.** The payload a `ret2` hands out is owned by the
caller: it carries one count of its own, the same count the box's slot would
have held for a caller that received the box. A function that returns a
whole result box as the pair (`return x` for `x: T!`) has to produce that
count from the box it holds, and how depends on who owns the box
(`ir_emit_return_box_as_pair`):

- A box the function built (the value of a call) is its own. The payload
  takes over the count the box held on it, and the destructor-less shell is
  freed.
- A borrowed box (a parameter, a field, an element, a match binding over
  one: whatever `ir_expr_is_borrowed` says the box return would retain)
  belongs to the caller or to the container. The box keeps its count on the
  payload, the payload is retained by its kind (the ok kind on the ok arm,
  the error kind on the error arm), and the shell is not touched.
- A tracked local holds one count on its box, but that box may be shared
  (`y := x` over a parameter). The return reads `pith_struct_strong_count`:
  when the local's count is the only one, it is the first case; otherwise
  the payload is retained by its kind and the local's count on the shell is
  released, the same decision the release of a result argument makes before
  it drops a payload.

Taking the payload out of a box that someone else still holds, without a
count of its own, left the caller's box and the returned payload sharing one
count, and whichever was released first freed what the other still read
(#1164).

**A returned box belongs to the caller alone.** Every caller that opens a
call's box takes over the payload's count, which is correct only when
nothing else holds the box. So a function that returns the box itself (the
box ABI, which generic functions and most interface impl methods use) and
returns a box it does not fully own (a parameter, a field, an element, or a
local whose count is shared) passes it through `__result_unshare_<ok><err>`.
That helper returns the same box when the caller's count is the only one,
with its destructor cleared. Otherwise it returns a fresh box holding its own
count on the live payload, gives the shared box the payload destructor, and
drops the count on it. The register return clears the destructor the same
way when a local's box is its own and the payload leaves with its count.

**main's result.** `fn main() -> Int` returns its `Int` value as the process
exit status; any non-`Int` `main` (void, `String`, etc.) pins its normal exit
status to `0` so heap pointers are never truncated into exit codes. A
`fn main() -> T!` settles its result before the `ret`: a returned result that
holds an error is reported through `pith_main_error_exit` (exit status 1), an
ok `Int` in `fn main() -> Int!` is returned as the process exit status, and any
other ok payload is released before exiting `0`. A `fail` or a `!` in main
reports the same way, before any box is built.

**Optional encoding.** A `T?` is the null pointer when it is none. A Some is
a two-slot struct allocation holding 1 at offset 0 and the payload at offset
8, and it may carry a destructor that releases the payload. A none costs no
allocation, and releasing one is a no-op. A runtime function that returns an
optional builds it with `optional_tuple`, which returns 0 for none.

**Result encoding.** `result_int` and `result_bool` use a zero sentinel for the
error case: a real value `v` is carried as `v + 1`, and `0` means "error". The
consumer's `normalize_runtime_result` applies this. It is the reason a runtime
function that can fail returns 0 for failure and its real (nonnegative) value
plus one otherwise.

The sentinel only works for a builtin whose success can never be a raw 0. One
whose success can be 0 returns the `(is_ok, payload)` pair in two registers
instead, reached by `rcall`, so the error carries the runtime's own message:
`parse_int`, `parse_float` (#1140), and the byte-count writes `file_write`,
`file_write_bytes`, `tcp_write`, `tcp_write_bytes`, `process_write`,
`process_write_bytes`, `byte_buffer_write` and `byte_buffer_write_string_utf8`,
where an empty write is a success of 0 (#1153). The payload is the ok value
(a float as its bits) when `is_ok` is 1, and an owned error string when it is
0; the caller releases that string on its error arm. `ir_builtin_result_retkind`
in `ir_metadata.pith` is the list, and names them `tuple` because that is the
kind of the box a call site folds the pair into wherever it keeps the result
whole (a bound local, an argument, a field read). A call site that consumes
the pair on the spot (`!`, `catch`, a `catch:` block, `unwrap_or`, a pair
return) builds no box at all (#1162).

The runtime side is a `#[repr(C)]` struct of two `i64`s
(`ffi_util::ResultPair`). Every supported target returns such a struct in two
integer registers, and they are the registers a Cranelift signature with two
`I64` returns reads: `rax:rdx` under the x86-64 System V ABI, and `x0:x1`
under AAPCS64 on Linux and Apple's aarch64 variant on macOS. That is also why
a float payload travels as bits: AAPCS64 returns a mixed `{i64, f64}` struct
in `x0:x1`, where a Cranelift `(I64, F64)` signature would read `x0` and `v0`.
Windows x64 returns a 16-byte struct through a hidden pointer, so
`runtime_imports.rs` refuses to declare a pair-returning runtime function under
any calling convention other than System V or Apple aarch64.

**Struct construction.** A struct is built with an ordinary call whose function
name is a declared struct: `call REG StructName NFIELDS f0 f1 ...`. The consumer
recognizes the name as a struct, allocates with `pith_struct_alloc(NFIELDS)`, and
stores each argument at offset `i * 8`. There is no dedicated construction
instruction; the struct declaration is what makes the call special.

**Runtime functions.** The set of runtime functions and their machine signatures
lives in `cranelift/runtime-abi/runtime_functions.txt`, one
`key | symbol | params | returns` row each. `returns` is empty (void), one
type, or `I64,I64` for a builtin that returns the result pair above; `build.rs`
rejects any other multi-value return. `cranelift/codegen/build.rs`
turns that file into the import table the consumer declares. The emitter has its
own, separate notion of which runtime calls exist and what kind they return
(`ir_builtin_result_retkind`, `ir_method_tables`). The two are not yet a single
source of truth.

## Consumer-side rewrites (emitter knowledge that currently leaks downstream)

The consumer makes a handful of decisions today that depend on information the
emitter already has. They are documented here because they are part of the
real contract, and because they are the natural candidates for moving into the
emitter later.

- **`__list_get` / `__index` on a string register becomes `char_at`.** The
  consumer inspects whether the indexed register holds a string and rewrites the
  call. The emitter knows the receiver's type at emission time.
- **Struct construction**, above: the consumer re-derives that a call names a
  struct.

These are distinct from code-generation choices that belong in the back end
regardless: the consumer inlines `bytes_get`, `bytes_get_strict`, `byte_at`,
`string_len`, `pith_list_get_value(_unchecked)` and `pith_list_get_value_strict`
as fast paths (a header read instead of a call), falling back to the runtime
helper. Inlining is the back end's job; the *name rewrites and type dispatch*
above are not.

The two strict getters, what `bytes[i]` and `xs[i]` lower to, keep their abort
semantics through the fallback. The inline path performs the runtime's own
handle test (non-null, aligned, magic word intact), the element-size check for
lists, and one unsigned bounds compare, and anything that fails any of them
calls the runtime function, which re-runs its checks and prints the diagnostic
it always printed. `tests/aborts` pins those diagnostics and exit codes. An
earlier attempt at inlining `xs[i]` (August 2026) removed 30% of the
instructions of a self-hosted type-check and still measured 1-3% slower on wall
clock, so the shape here was accepted only after an interleaved A/B of
`pith_main check self-host/pith_main.pith` came out neutral (21 pairs, medians
within 0.6%). A change to this fast path needs that measurement again; an
instruction count is not enough.

## The robustness contract

The consumer rejects malformed function bodies loudly, with a `CompileError`
that names the function and the offending line. Tests in `ir_consumer.rs`
cover these cases:

- An unknown instruction opcode
- A call to a function that was never declared
- A `brif` or `jmp` to a label that does not exist
- A `label` line with anything after its name other than `cold`
- A malformed `store`, `call`, or `field`
- A register reference that was never assigned, or a non-numeric register token
- A `func` whose name collides with a runtime declaration
- A `call` whose retkind promises a value when the callee returns nothing. Every
  IR-declared function returns a value (one i64, or a two-value register pair),
  so this can only mean the emitter's idea
  of a runtime call disagrees with `runtime_functions.txt`. Before the check,
  the destination register got a zero nobody wrote

The driver checks the emitter's side of the same agreement before the consumer
sees it: `ir_driver --combined --validate <file>` refuses a call whose return
kind names a struct without the `struct:` prefix, a function defined more than
once, and the other declaration rules the driver knows. `make
validate-ir-contract-only` runs it over every corpus program with a `main`, and
CI runs that target. It exists because a call to a function in another module
that returns a struct defined in a third module used to spell the kind bare:
the contract was violated in 22 corpus programs and, since the classifier saw
nothing to release, every such returned struct leaked (#1057).

It also refuses a `strref` that names no `string` line anywhere in the
combined file. Each module's string table is written before its bodies, so a
literal a body interns for the first time lands after the table has been
emitted; the consumer reads such a reference as a null string (see the
soft spots below), and the text goes missing without a diagnostic. That is how
`parse_float`'s error message came out empty (#1147) and how an index miss in
a fallible function lost its `index out of bounds` (#1109). A string a body
refers to belongs in the string prepass in `emit_ir`.

These are the known soft spots, where the consumer is quieter than it should
be. They are the targets for tightening, not settled behavior:

- An **unknown declaration** in the first pass is silently ignored instead of
  rejected.
- If `pith_struct_alloc` is somehow absent, a struct construction falls back to a
  zero register instead of failing.
- A `strref` naming no string reads as a null string instead of failing,
  because a single-module dump may name a string another compilation unit
  defines. `--validate` refuses it in a combined file, which is where every
  string the program uses is defined.

## Sources of truth

| Concern | File |
|---------|------|
| Producing the IR | `self-host/ir_emitter_core.pith` and its satellites |
| Consuming the IR | `cranelift/codegen/src/ir_consumer.rs` |
| Runtime function signatures | `cranelift/runtime-abi/runtime_functions.txt` → `build.rs` |
| What runtime functions do with counts | `cranelift/runtime-abi/runtime_ownership.txt` → `build.rs` |
| This contract | This file |
