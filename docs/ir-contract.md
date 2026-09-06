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
`load`) are the mutable slots that survive across basic blocks; registers do not.

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
```

**calls**

```
call REG FNAME RETKIND NARGS ARG...
rcall FLAG_REG VAL_REG FNAME PAYLOAD_KIND NARGS ARG...
```

`call` covers everything that returns a single value: runtime functions, user
functions, struct construction, and void calls. `RETKIND` names how to treat the
result (see the type vocabulary). a void call is a `call` with a `void` retkind
whose result register is simply never read — there is no separate `callv`.

`rcall` is the register-result form. it calls a function whose retkind is
`result_reg` or `result_reg_f` and lands the two returned values in two
registers: `FLAG_REG` takes the ok flag, `VAL_REG` the payload, interpreted as
`PAYLOAD_KIND`.

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
```

**control flow**

```
ret REG                     return REG from the current function.
ret2 FLAG_REG VAL_REG       return both registers, under the register-result abi.
brif COND THEN ELSE         branch to label THEN if COND is nonzero, else ELSE.
jmp LABEL                   unconditional branch.
label NAME                  a branch target.
```

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
and `RETTYPE` does drive the machine signature for those two kinds.

**result encoding.** `result_int` and `result_bool` use a zero sentinel for the
error case: a real value `v` is carried as `v + 1`, and `0` means "error". the
consumer's `normalize_runtime_result` applies this. it is the reason a runtime
function that can fail returns 0 for failure and its real (nonnegative) value
plus one otherwise.

**struct construction.** a struct is built with an ordinary call whose function
name is a declared struct: `call REG StructName NFIELDS f0 f1 ...`. the consumer
recognizes the name as a struct, allocates with `pith_struct_alloc(NFIELDS)`, and
stores each argument at offset `i * 8`. there is no dedicated construction
instruction — the struct declaration is what makes the call special.

**runtime functions.** the set of runtime functions and their machine signatures
lives in `cranelift/runtime-abi/runtime_functions.txt`, one
`key | symbol | params | returns` row each. `cranelift/codegen/build.rs`
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
end regardless: the consumer inlines `bytes_get`, `byte_at`, `string_len`, and
`pith_list_get_value(_unchecked)` as fast paths (a header read instead of a call),
falling back to the runtime helper. inlining is the back end's job; the *name
rewrites and type dispatch* above are not.

`pith_list_get_value_strict` — what `xs[i]` actually lowers to, and the single
hottest call in the language — is deliberately **not** inlined. it looks like the
obvious next candidate and it has been measured: an inline fast path removes about
30% of all instructions executed and half of all memory references when
type-checking the self-hosted compiler, and still comes out 1-3% *slower* on wall
clock. the shared runtime function is a hot micro-kernel that stays resident in
L1i with perfectly trained branches, and spreading it across thousands of call
sites costs more in instruction fetch and branch-predictor pressure than the call
saves. trimming the shared kernel instead was worth 11%. do not re-litigate this
without an interleaved A/B on a real workload — the instruction count will lie.

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

known soft spots, where the consumer is quieter than it should be — these are the
tightening targets, not settled behavior:

- an **unknown declaration** in the first pass is silently ignored rather than
  rejected.
- a `func` whose name collides with a runtime declaration is silently skipped.
- if `pith_struct_alloc` is somehow absent, a struct construction falls back to a
  zero register instead of failing.

## sources of truth

| concern | file |
|---------|------|
| producing the ir | `self-host/ir_emitter_core.pith` and its satellites |
| consuming the ir | `cranelift/codegen/src/ir_consumer.rs` |
| runtime function signatures | `cranelift/runtime-abi/runtime_functions.txt` → `build.rs` |
| this contract | this file |
