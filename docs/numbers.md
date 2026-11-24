# numbers

pith has four numeric types. three of them are exact and one of them is not, and
knowing which is which is most of this page.

| type | holds | exact | cost |
| --- | --- | --- | --- |
| `Int` | whole numbers, 64 bits, two's complement | yes; overflow stops the program | a machine register |
| `Float` | ieee-754 binary double | no | a machine register |
| `std.bigint.BigInt` | whole numbers, any size | yes | a heap value; one machine word of magnitude under 10^18 |
| `std.decimal.Decimal` | decimal fractions, any size | yes | a heap value wrapping a BigInt |

the short version: `Int` for counts, indices, ids, and bytes. `Float` for
measurements, ratios, and anything headed for a chart. `Decimal` for money, tax,
invoice lines, and anything a person will later reconcile against a statement.
`BigInt` when a whole number outgrows 64 bits.

## literals

an integer literal is an `Int`, written in decimal, hex (`0xff`), binary
(`0b1010`) or octal (`0o17`), with `_` allowed between digits (`1_000_000`).
it has to fit: `9223372036854775807` is the largest and
`-9223372036854775808` the smallest, and a literal past either is a compile
error (E319). a literal in a `match` arm takes the type of the value it is
compared against, so a `UInt` arm can name `18446744073709551615`.

## overflow

`+`, `-` and `*` on an `Int` stop the program when the result does not fit in
64 bits, and so does `-x` on the most negative `Int`:

```
pith runtime error: integer overflow
```

`UInt` and `UInt64` stop the same way when a result leaves 0 to 2^64 - 1, so
`UInt(3) - UInt(5)` is an error rather than a very large number. the narrow
types (`Int8`, `Int16`, `Int32`, `UInt8`, `UInt16` and `UInt32`) stop when a
result of `+`, `-`, `*`, `/` or `-x` leaves the type's range, so
`Int8(127) + Int8(1)` and `Int8(-128) / Int8(-1)` are errors too.

the rest of the integer operations follow the same rule:

| operation | stops the program when |
| --- | --- |
| `Int8(x)`, `Int16(x)`, `Int32(x)`, `UInt8(x)`, `UInt16(x)`, `UInt32(x)` | `x` is outside the target type's range |
| `UInt(x)`, `UInt64(x)` | `x` is negative |
| `Int64(x)` | never; every `Int` fits |
| `abs(x)` | `x` is the most negative `Int` |
| `f.to_int()` | `f` is NaN, or outside the `Int` range once truncated toward zero |
| `bit_shl(x, n)`, `bit_shr(x, n)` | `n` is outside 0 to 63 |

the error is `integer overflow` in every case except NaN, which reports
`NaN converted to an integer`. a shift drops any bits it moves past either
end without an error, so `bit_shl(x, 1)` on a large `x` loses its top bit.
the rotates in `std.bits` take their amount modulo the width.

hashing, checksums and random number generators want the wrapping result. they
ask for it by name:

```pith
h = h.wrapping_mul(1099511628211)
seed = seed.wrapping_add(1)
```

`wrapping_add`, `wrapping_sub` and `wrapping_mul` take a value of the
receiver's type and return one, wrapping at 64 bits for `Int`, `UInt` and
`UInt64`, and into the type's range for the narrow ones. when the number is
meant to grow without bound, use `std.bigint`.

a conversion that should keep the low bits asks for it by name too.
`wrapping_int8()`, `wrapping_int16()`, `wrapping_int32()`, `wrapping_uint8()`,
`wrapping_uint16()`, `wrapping_uint32()`, `wrapping_uint()` and
`wrapping_uint64()` work on any integer and keep the bits of its two's
complement value that fit the target type:

```pith
print("{h.wrapping_uint64()}")      # a 64-bit hash, printed unsigned
low := (300).wrapping_uint8()       # 44
```

masking first works as well when the target is unsigned:
`UInt8(bit_and(x, 255))`.

`examples/overflow_wrapping.pith` uses them for a hash, a random number
generator and a sequence number, and checks a sum before it can overflow.

## text to a number

`parse_int(text)` returns `Int!String` and `parse_float(text)` returns
`Float!String`. both trim surrounding whitespace first.

`parse_float` accepts an optional sign, digits with an optional fraction
(`5`, `5.`, `.5`, `5.25`) and an optional exponent (`2.5e3`, `1E-9`). every
finite value is a success, zeros included: `0`, `0.0` and `-0.0` parse, a
negative zero keeps its sign, and a value too small for a `Float` (`1e-400`)
rounds to a zero of the text's sign. the errors are:

| text | error |
| --- | --- |
| not a number (`abc`, `1.5x`, the empty string) | `invalid float` |
| an exponent past a Float's range (`1e400`, `-1e400`) | `float out of range` |
| a NaN or an infinity spelled out (`nan`, `inf`, `Infinity`) | `float is not finite` |

so a successful `parse_float` is always a finite number. `std.env.float_or` and
`std.config.get_float_or` read through it and return their fallback exactly
when it fails. `parse_int`'s errors are `invalid integer` and `integer
overflow`.

## why money is never a Float

a `Float` stores a binary fraction. `0.1` is not a binary fraction, so a Float
cannot hold it — it holds the nearest one it can, which is slightly off. the
error is invisible in one value and accumulates across many:

```pith
mut total := 0.0
mut count := 0
while count < 10:
    total = total + 0.1
    count = count + 1
print("{total}")          # 1, printed to 15 significant digits
print("{total == 1.0}")   # false
```

the same loop over `Decimal` gives `1.00`, because a decimal fraction is what it
stores.

the second failure is size. a `Float` has 53 bits of mantissa, so past 2^53 it
stops being able to name consecutive integers. a ledger balance of
`12345678901234567.89` — comfortably inside a `NUMERIC(20,2)` column — comes back
from a Float as `12345678901234568`. the cents are gone and the integer part is
off by one, and nothing anywhere reports an error.

neither failure announces itself. that is what makes it worth a type rather than
a warning in a comment.

`examples/money.pith` runs both versions side by side.

## std.decimal

a `Decimal` is an arbitrary-precision integer and a scale: the value is
`unscaled / 10^scale`. this is java's `BigDecimal`, and it is also exactly what
sql `NUMERIC(p, s)` means, which is why a database column round-trips through it
without losing a digit.

```pith
import std.decimal as decimal

price := decimal.parse("19.99")!
quantity := decimal.from_int(3)
subtotal := decimal.mul(price, quantity)          # 59.97
tax := decimal.mul(subtotal, decimal.parse("0.0825")!)
total := decimal.add(subtotal, decimal.rescale(tax, 2, decimal.half_even())!)
print(decimal.to_string(total))                   # 64.92
```

### the scale is part of the value

`1.10` parses to an unscaled 110 at scale 2 and prints back as `1.10`, not `1.1`.
that is deliberate: a `NUMERIC(10,2)` column stores the trailing zero and prints
it, so keeping it is what makes the round-trip byte-identical.

comparison ignores the scale — `1.0` and `1.00` are equal — so the scale never
changes an answer, only how the value reads. `strip_trailing_zeros` drops it when
the shorter form is what you want.

addition and subtraction take the wider of the two scales, so no digit of either
operand is dropped. multiplication takes the sum of the two scales, because that
is what an exact product needs: `1.05 * 1.05` is `1.1025` at scale 4. rescale it
when you want it back at scale 2, and say how it should round.

### there is no conversion from Float

on purpose. java's `new BigDecimal(0.1)` gives
`0.1000000000000000055511151231257827021181583404541015625`, which is the exact
answer and almost never the wanted one. the alternative — taking the decimal a
Float was *printed* as and treating it as exact — hides the error rather than
removing it. neither belongs behind a function that looks safe.

if you genuinely hold a `Float` and want a `Decimal`, format it and parse the
text, so the lossy step is the one you wrote:

```pith
approximate := decimal.parse(fmt_float(measurement, 4))!
```

`to_float` goes the other way and is documented as lossy, because printing and
plotting are real needs.

### division says how it rounds

division is the only operation here that can be inexact — `1/3` has no decimal
form — so `div` takes the scale and the rounding mode at the call site. there is
no default, because a default is how a fraction of a cent goes missing:

```pith
share := decimal.div(total, decimal.from_int(3), 2, decimal.half_even())!
```

the seven modes are `down` (truncate toward zero), `up` (away from zero), `floor`
(toward negative infinity), `ceiling` (toward positive infinity), `half_up`,
`half_down`, and `half_even`.

**`half_even` is the one to reach for with money.** it sends an exact midpoint to
whichever neighbour ends in an even digit, so midpoints go up and down about
equally often and a long run of transactions does not drift. `half_up` — the
rounding taught in school — pushes every midpoint away from zero. rounding
`0.125`, `0.135`, `0.145` and `0.155` to cents sums to `0.58` under `half_up` and
to `0.56` under `half_even`; the exact total is `0.56`.

when you expect a division to come out even, say so:

```pith
half := decimal.div_exact(amount, decimal.from_int(2))!
```

`div_exact` computes the quotient exactly and fails when it does not terminate,
so an assumption that stops holding surfaces as an error rather than as a quiet
rounding. it keeps at least as many places as the dividend had beyond the
divisor, so `10.00 / 2` is `5.00` rather than `5`, and no more than it needs, so
`1.00 / 8` is `0.125`.

### splitting an amount without losing a cent

dividing `10.00` three ways and rounding each share gives `3.33` three times,
which is `9.99`. `allocate` hands the leftover minor units out one at a time so
the parts sum back to exactly the total:

```pith
shares := decimal.allocate(decimal.parse("10.00")!, [1, 1, 1])!
# 3.34, 3.33, 3.33
```

the weights are integer proportions, so `[3, 7]` splits 30/70.

## std.bigint

`BigInt` is the arbitrary-precision integer `Decimal` is built on, and it stands
alone for whole numbers that outgrow 64 bits.

```pith
import std.bigint as bigint

value := bigint.parse("170141183460469231731687303715884105727")!
print(bigint.to_string(bigint.mul(value, value)))
```

`add`, `sub` and `mul` are exact. `divmod` truncates toward zero and gives the
remainder the sign of the dividend, matching pith's own `/` and `%` on `Int`, and
fails on a zero divisor rather than returning a fabricated value.

a magnitude of at most 18 decimal digits lives in a machine `Int` inside the
value, with no list and no loop, so ordinary-sized arithmetic does not pay for
the general path.

two things it deliberately does not do:

- **no bitwise or shift operations.** the limb base is 10^9 rather than a power
  of two, which makes decimal conversion and scaling by a power of ten linear —
  the workload this type exists for — at the cost of making bit twiddling
  expensive. `std.bits` operates on `Int` and is the right tool for that.
- **not for cryptography.** nothing here is constant-time and there is no modular
  exponentiation. key material and signatures belong in `std.crypto`, which is
  backed by a reviewed implementation.

## numbers out of a database

a `NUMERIC` (postgres) or `DECIMAL` (mysql) column decodes to
`std.sql.Value.Numeric`, carrying a `Decimal`. read it with `row.decimal(name)`:

```pith
import std.db.postgres as db
import std.decimal as decimal

handle := db.open("postgres://me:pw@127.0.0.1:5432/app")!
mut balance := decimal.zero()
for row in handle.query("select amount from ledger", [])!:
    balance = decimal.add(balance, row.decimal("amount"))
print(decimal.to_string(balance))
```

`float4` and `float8` still decode to `Value.Real`, because those genuinely are
floats. reading a numeric column with `row.float(name)` still works and still
rounds — the coercion is there so code written before `Numeric` existed keeps
compiling, not because it is a good idea.

bind a decimal back by its text, which is exact:

```pith
handle.exec("insert into ledger (amount) values ($1)", [decimal.to_string(amount)])!
```

a value the driver cannot parse as a number — postgres sends `NaN`, `Infinity`
and `-Infinity` for a numeric — decodes to `Value.Text` holding the original
string. it is never turned into a zero. a `float4` or `float8` column holds
those three as values, so there they decode to `Value.Real` with the NaN or
the infinity itself.

see [docs/db.md](db.md) for the rest of the database surface.

## limits

`Decimal` caps its scale at 16383, which is postgres's own ceiling on a numeric's
display scale, and `parse` refuses an exponent that would expand past a million
digits rather than letting untrusted input turn into an allocation.
`bigint.ten_pow` and `bigint.pow` refuse a result past 1.8 million digits for the
same reason. these are types for money and measurements, not for computing a
constant to a million places.
