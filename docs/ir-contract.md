# the pith ir contract

the pith compiler is split in two. the self-hosted front end (`self-host/*.pith`)
type-checks a program and lowers it to a small text ir; the rust back end
(`cranelift/codegen/src/ir_consumer.rs`) reads that text and hands it to
cranelift for native code. this document is the contract between them — the
instruction set, the type vocabulary, the abi conventions, and the rules about
what the consumer accepts and what it rejects.

it is meant to be authoritative. when the emitter and the consumer disagree, one
of them has a bug; this file says what the agreement is supposed to be.

## the shape of the ir

the ir is line-oriented text. one instruction per line, tokens separated by
single spaces, no punctuation. the consumer splits each line on whitespace and
dispatches on the first token. blank lines are ignored.

there are two kinds of line: **declarations** that set up the module (strings,
functions, structs, globals) and **instructions** that make up a function body.
the consumer reads the whole file twice — once to declare every function and
piece of data so calls can resolve regardless of order, then again to compile
each function body.

values inside a body live in **registers**, written as bare integers (`1`, `2`,
`8`). a register is assigned once by the instruction that produces it and read by
later instructions. registers are function-local. named **variables** (`store` /
`load`) are the mutable slots that carry values across basic blocks. a register
can be read in a later block only when the block that defines it runs on every
path there: the inlined `contains` search, for one, keeps its list and length in
registers across its loop. a register defined on just one branch is gone after
the branches meet.

## declarations

these appear at module scope. the consumer reads them in the first pass.

```
string N "content"          a string literal. N is its index; strref refers back to it.
func NAME NPARAM RETTYPE     opens a function. NPARAM is the parameter count.
param NAME                   names one parameter, in order (inside the func body).
endfunc                      closes the current function.
struct NAME FIELD...         declares a struct's field layout.
struct_alias ALIAS TARGET    makes ALIAS resolve to TARGET's layout.
global NAME INIT_KIND ...    a module-level global and how to initialize it.
```

one wrinkle worth stating plainly: **a function is lowered as `nparam` i64
parameters returning one i64, except under the register-result abi**, where it
returns two values (see abi conventions below). apart from that case `RETTYPE`
is descriptive, and the real return handling is driven per-call by the call's
retkind.

## instructions

these make up a function body, between `func` and `endfunc`.

**constants and string references**

```
iconst REG VALUE            integer constant.
fconst REG VALUE            float constant (stored bitcast into an i64 register).
strref REG STRIDX           load a pointer to string literal STRIDX.
```

**integer arithmetic**

```
add|sub|mul|div|mod REG A B   REG = A op B. div and mod are checked (a zero
                              divisor traps through the runtime, not silently).
```

**float arithmetic**

```
fadd|fsub|fmul|fdiv REG A B   float op on two float registers.
```

`add|sub|mul|div` also lower as float ops when both operands are float registers —
the consumer tracks which registers hold floats and picks the instruction class
accordingly.

**bitwise and logical**

```
band|bor|bxor|shl|shr REG A B    bitwise ops.
bnot REG A                       bitwise complement.
and|or REG A B                   lowered as bitwise band/bor (operands are 0/1).
```

**comparison**

```
eq|neq|lt|gt|lte|gte REG A B     result is 0 or 1. dispatches on operand kind:
                                 integers compare directly, floats via FloatCC,
                                 strings via the pith_cstring_* runtime helpers.
```

**strings**

```
concat REG A B              REG = A followed by B, as a fresh heap string.
concatn REG COUNT PART...   REG = the COUNT parts joined, as one fresh string.
appendn REG COUNT BASE PART...   REG = BASE with the COUNT parts appended; the
                                 result takes over BASE's count.
```

the emitter writes only `concat`. `concatn` and `appendn` are made inside the
consumer, by `fuse_concat_chains` and `fuse_self_appends`, from a chain of
`concat`s whose intermediate results nothing else reads.

**optionals**

```
optflag REG OPT             1 when the optional OPT holds a value, 0 when it
                            is none.
optval REG OPT KIND NAME    the payload of OPT, typed by KIND, or 0 when OPT
                            is none.
```

the emitter reads optionals through these two forms only, never with `field`,
because a none has no shell to load from (see the optional encoding below).

**calls**

```
call REG FNAME RETKIND NARGS ARG...
rcall FLAG_REG VAL_REG FNAME PAYLOAD_KIND NARGS ARG...
```

`call` covers everything that returns a single value: runtime functions, user
functions, struct construction, and void calls. `RETKIND` names how to treat the
result (see the type vocabulary). a void call is a `call` with a `void` retkind
whose result register is simply never read — there is no separate `callv`.

`rcall` is the register-result form. it calls a function that returns an
`(is_ok, payload)` pair and lands the two values in two registers: `FLAG_REG`
takes the ok flag, `VAL_REG` the payload, interpreted as `PAYLOAD_KIND`. the
callee is either a declared function whose retkind is `result_reg` or
`result_reg_f`, or a runtime builtin whose row in `runtime_functions.txt`
returns `I64,I64` (see the result encoding below). a `float` payload from a
runtime builtin arrives as the f64's bits in the second integer word, where a
`result_reg_f` function returns an actual f64; the consumer handles both. a
plain `call` to a pair-returning callee is a consumer error, as is an `rcall`
to anything else, and `ir_driver --validate` rejects both before the consumer
sees them.

**memory and fields**

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

`VARNAME` is one flat namespace and the consumer resolves it against the
module-level globals before the function's own variables, so a global and a
local of the same spelling would name the same storage. the emitter keeps them
apart by spelling: a local, a parameter and a pattern binding reach the ir under
the name they were written with, while a global is declared and reached under
`__g_<name>`. nothing written in a source file produces that spelling, so the
two can no longer meet, and `ir_global_storage_symbol` is the single place the
mapping is applied.

**function values**

```
funcref REG NAME            REG = a pointer to function NAME.
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

**control flow**

```
ret REG                     return REG from the current function.
ret2 FLAG_REG VAL_REG       return both registers, under the register-result abi.
brif COND THEN ELSE         branch to label THEN if COND is nonzero, else ELSE.
jmp LABEL                   unconditional branch.
label NAME                  a branch target.
```

## count marks

with `PITH_CHECK_COUNTS=1` set, the emitter adds `mark` lines for the count
checker. they state facts the instructions leave out: a load or a `param`
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

a function's `kind`, `owns`, `mixed` and `untracked` marks come just before
its `endfunc`. a loop's marks come where the loop sets up its slots. a
slot with no mark is one the emitter made for a scalar, such as the flag a
short-circuit `and` keeps. functions the emitter makes itself, such as
destructors and enum equality, carry none.

a user function borrows every parameter: the caller keeps its count. a
parameter with an `owns` mark is one the function's prologue retains, and
its exit cleanup gives that count back. runtime functions differ case by
case, and `cranelift/runtime-abi/runtime_ownership.txt` says how; `build.rs`
refuses a runtime function without a row there.

`PITH_CHECK_COUNTS=1 pith build file.pith` also runs the count checker
(`cranelift/codegen/src/count_check.rs`) on the marked ir before the
backend drops the marks. it prints, on stderr, how many functions balance,
how many don't, and how many it couldn't settle, with each unbalanced
function and the instruction that made the count in question. it checks two
rules:

- balance: on every path, each count a function takes is given up exactly
  once, by a release, a store into a holder, or the return.
- borrow: a loop variable marked `borrow` is not read after the loop body
  could have dropped what it borrows from, through an evicting method such
  as `clear` or `pop`, a store into the same storage, or a call into a
  function that writes it. a call the checker knows nothing about counts as
  a write.

`PITH_CHECK_COUNTS=2` adds the join details and the names of the balanced
functions. `PITH_CHECK_COUNTS=only` reports and stops before code
generation, for tools that want only the report.

the build itself never fails on a finding. `make count-marks-check-only`,
which ci runs, checks every corpus program and example and fails on any
unbalanced function or borrow finding. a function the checker can't settle,
such as one holding a weak reference, is counted as undecided and doesn't
fail the target. over the corpus, 7051 distinct functions balance and 194
are undecided; the rest of the undecided are values of unknown kind, calls
with no summary, weak references and locals whose kind changes by path.

the checker models the register-pair abi (`rcall`, `ret2`): the payload holds
one count, the ok value's or the error's, and a plain ok payload holds it
only when the flag is clear. it reads a result box's `ok` or `err` slot as a
payload the box hands over when it is freed, an optional's `optval` as a
field read, and a channel receive's flag as a null test that follows the
value into locals.

the checker was tested by mutation: delete one release, or duplicate one, in
a function it balances, and see whether it flags that function. on
`examples/api_gateway`, where 3579 functions balance, it caught 299 of 300
mutants and 499 of 500 over two seeds; on `tests/cases/test_process_command`
it caught all 300. the misses are a doubled release of a channel receive's
optional after it was stored, and a deleted release of a result box after its
`ok` payload was read.

marks generate no code. the consumer drops every `mark` line before its first
pass, so a mark can't split a pattern its rewrites match, and a build with
marks produces the same object code as one without. without the variable, no
marks are written, and the dumped ir and the bootstrap seed are unchanged.

## the type vocabulary

types travel through the ir as short **kind strings**, not as structured data.
the same vocabulary shows up as a call's `RETKIND`, a `field` kind, and a
global's init kind:

| kind | meaning |
|------|---------|
| `int`, `bool` | a plain integer register |
| `float` | a float, bitcast through an i64 register |
| `string` | a heap or literal c-string pointer |
| `bytes` | a byte buffer pointer |
| `list`, `map`, `set` | a collection handle |
| `struct` | a struct pointer of unspecified type |
| `struct:NAME` | a struct pointer known to be of type NAME |
| `tuple` | a tuple (a struct-shaped allocation) |
| `result`, `result_int`, `result_bool` | a fallible result (see encoding below) |
| `result_reg`, `result_reg_f` | a fallible result returned in registers: `(is_ok, payload)`, the payload i64 or f64 |
| `optional` | an optional value |
| `void` | no meaningful result |
| `unknown` | the emitter did not commit to a kind |

`unknown` is an explicit admission, not a wildcard: it means the consumer will not
track the register's kind and later kind-specific handling (string release, float
math, struct field access) will not fire for it. an `unknown` where a concrete
kind was knowable is a missed optimization or a latent bug, not a convenience.

## abi conventions

**i64 parameters, one or two returns.** parameters are always i64: pointers are
i64, and floats are bitcast to i64 across call boundaries and cast back where
float math is needed. returns are i64 too, except for the register-result abi.
a function whose retkind is `result_reg` returns two values, `(is_ok: i64,
payload: i64)`; `result_reg_f` returns `(is_ok: i64, payload: f64)`. everything
else returns a single i64. `push_return_types` in the consumer is the authority,
and `RETTYPE` does drive the machine signature for those two kinds. the runtime
builtins that return the same pair take their signature from their table row
instead (see the result encoding below).

**who owns a returned pair.** the payload a `ret2` hands out is owned by the
caller: it carries one count of its own, the same count the box's slot would
have held for a caller that received the box. a function that returns a
whole result box as the pair (`return x` for `x: T!`) has to produce that
count from the box it holds, and how depends on who owns the box
(`ir_emit_return_box_as_pair`):

- a box the function built (the value of a call) is its own. the payload
  takes over the count the box held on it, and the destructor-less shell is
  freed.
- a borrowed box (a parameter, a field, an element, a match binding over
  one: whatever `ir_expr_is_borrowed` says the box return would retain)
  belongs to the caller or to the container. the box keeps its count on the
  payload, the payload is retained by its kind (the ok kind on the ok arm,
  the error kind on the error arm), and the shell is not touched.
- a tracked local holds one count on its box, but that box may be shared
  (`y := x` over a parameter). the return reads `pith_struct_strong_count`:
  when the local's count is the only one, it is the first case; otherwise
  the payload is retained by its kind and the local's count on the shell is
  released, the same decision the release of a result argument makes before
  it drops a payload.

taking the payload out of a box that someone else still holds, without a
count of its own, left the caller's box and the returned payload sharing one
count, and whichever was released first freed what the other still read
(#1164).

**optional encoding.** a `T?` is the null pointer when it is none. a Some is
a two-slot struct allocation holding 1 at offset 0 and the payload at offset
8, and it may carry a destructor that releases the payload. a none costs no
allocation, and releasing one is a no-op. a runtime function that returns an
optional builds it with `optional_tuple`, which returns 0 for none.

**result encoding.** `result_int` and `result_bool` use a zero sentinel for the
error case: a real value `v` is carried as `v + 1`, and `0` means "error". the
consumer's `normalize_runtime_result` applies this. it is the reason a runtime
function that can fail returns 0 for failure and its real (nonnegative) value
plus one otherwise.

the sentinel only works for a builtin whose success can never be a raw 0. one
whose success can be 0 returns the `(is_ok, payload)` pair in two registers
instead, reached by `rcall`, so the error carries the runtime's own message:
`parse_int`, `parse_float` (#1140), and the byte-count writes `file_write`,
`file_write_bytes`, `tcp_write`, `tcp_write_bytes`, `process_write`,
`process_write_bytes`, `byte_buffer_write` and `byte_buffer_write_string_utf8`,
where an empty write is a success of 0 (#1153). the payload is the ok value
(a float as its bits) when `is_ok` is 1, and an owned error string when it is
0; the caller releases that string on its error arm. `ir_builtin_result_retkind`
in `ir_metadata.pith` is the list, and names them `tuple` because that is the
kind of the box a call site folds the pair into wherever it keeps the result
whole (a bound local, an argument, a field read). a call site that consumes
the pair on the spot (`!`, `catch`, a `catch:` block, `unwrap_or`, a pair
return) builds no box at all (#1162).

the runtime side is a `#[repr(C)]` struct of two `i64`s
(`ffi_util::ResultPair`). every supported target returns such a struct in two
integer registers, and they are the registers a Cranelift signature with two
`I64` returns reads: `rax:rdx` under the x86-64 System V ABI, and `x0:x1`
under AAPCS64 on linux and Apple's aarch64 variant on macOS. that is also why
a float payload travels as bits: AAPCS64 returns a mixed `{i64, f64}` struct
in `x0:x1`, where a Cranelift `(I64, F64)` signature would read `x0` and `v0`.
Windows x64 returns a 16-byte struct through a hidden pointer, so
`runtime_imports.rs` refuses to declare a pair-returning runtime function under
any calling convention other than System V or Apple aarch64.

**struct construction.** a struct is built with an ordinary call whose function
name is a declared struct: `call REG StructName NFIELDS f0 f1 ...`. the consumer
recognizes the name as a struct, allocates with `pith_struct_alloc(NFIELDS)`, and
stores each argument at offset `i * 8`. there is no dedicated construction
instruction — the struct declaration is what makes the call special.

**runtime functions.** the set of runtime functions and their machine signatures
lives in `cranelift/runtime-abi/runtime_functions.txt`, one
`key | symbol | params | returns` row each. `returns` is empty (void), one
type, or `I64,I64` for a builtin that returns the result pair above; `build.rs`
rejects any other multi-value return. `cranelift/codegen/build.rs`
turns that file into the import table the consumer declares. the emitter has its
own, separate notion of which runtime calls exist and what kind they return
(`ir_builtin_result_retkind`, `ir_method_tables`) — the two are not yet a single
source of truth.

## consumer-side rewrites (emitter knowledge that currently leaks downstream)

a handful of decisions are made by the consumer today even though they depend on
information the emitter already has. they are documented here because they are
part of the real contract, and because they are the natural candidates for moving
into the emitter later.

- **`__list_get` / `__index` on a string register becomes `char_at`.** the
  consumer inspects whether the indexed register holds a string and rewrites the
  call. the emitter knows the receiver's type at emission time.
- **struct construction**, above — the consumer re-derives that a call names a
  struct.

these are distinct from genuine code-generation choices that belong in the back
end regardless: the consumer inlines `bytes_get`, `bytes_get_strict`, `byte_at`,
`string_len`, `pith_list_get_value(_unchecked)` and `pith_list_get_value_strict`
as fast paths (a header read instead of a call), falling back to the runtime
helper. inlining is the back end's job; the *name rewrites and type dispatch*
above are not.

the two strict getters, what `bytes[i]` and `xs[i]` lower to, keep their abort
semantics through the fallback: the inline path performs the runtime's own
handle test (non-null, aligned, magic word intact), the element-size check for
lists, and one unsigned bounds compare, and anything that fails any of them
calls the runtime function, which re-runs its checks and prints the diagnostic
it always printed. `tests/aborts` pins those diagnostics and exit codes. an
earlier attempt at inlining `xs[i]` (august 2026) removed 30% of the
instructions of a self-hosted type-check and still measured 1-3% slower on wall
clock, so the shape here was accepted only after an interleaved A/B of
`pith_main check self-host/pith_main.pith` came out neutral (21 pairs, medians
within 0.6%); a change to this fast path needs that measurement again, not an
instruction count.

## the robustness contract

the consumer rejects malformed function bodies loudly, with a `CompileError` that
names the function and the offending line. these are covered by tests in
`ir_consumer.rs`:

- an unknown instruction opcode
- a call to a function that was never declared
- a `brif` or `jmp` to a label that does not exist
- a malformed `store`, `call`, or `field`
- a register reference that was never assigned, or a non-numeric register token
- a `call` whose retkind promises a value when the callee returns nothing. every
  ir-declared function returns an i64, so this can only mean the emitter's idea
  of a runtime call disagrees with `runtime_functions.txt`. before the check,
  the destination register got a zero nobody wrote

the driver checks the emitter's side of the same agreement before the consumer
sees it: `ir_driver --combined --validate <file>` refuses a call whose return
kind names a struct without the `struct:` prefix, a function defined more than
once, and the other declaration rules the driver knows. `make
validate-ir-contract-only` runs it over every corpus program with a `main`, and
CI runs that target. it exists because a call to a function in another module
that returns a struct defined in a third module used to spell the kind bare:
the contract was violated in 22 corpus programs and, since the classifier saw
nothing to release, every such returned struct leaked (#1057).

it also refuses a `strref` that names no `string` line anywhere in the
combined file. each module's string table is written before its bodies, so a
literal a body interns for the first time lands after the table has been
emitted; the consumer reads such a reference as a null string (see the
soft spots below), and the text goes missing without a diagnostic. that is how
`parse_float`'s error message came out empty (#1147) and how an index miss in
a fallible function lost its `index out of bounds` (#1109). a string a body
refers to belongs in the string prepass in `emit_ir`.

known soft spots, where the consumer is quieter than it should be — these are the
tightening targets, not settled behavior:

- an **unknown declaration** in the first pass is silently ignored rather than
  rejected.
- a `func` whose name collides with a runtime declaration is silently skipped.
- if `pith_struct_alloc` is somehow absent, a struct construction falls back to a
  zero register instead of failing.
- a `strref` naming no string reads as a null string instead of failing,
  because a single-module dump may name a string another compilation unit
  defines. `--validate` refuses it in a combined file, which is where every
  string the program uses is defined.

## sources of truth

| concern | file |
|---------|------|
| producing the ir | `self-host/ir_emitter_core.pith` and its satellites |
| consuming the ir | `cranelift/codegen/src/ir_consumer.rs` |
| runtime function signatures | `cranelift/runtime-abi/runtime_functions.txt` → `build.rs` |
| what runtime functions do with counts | `cranelift/runtime-abi/runtime_ownership.txt` → `build.rs` |
| this contract | this file |
