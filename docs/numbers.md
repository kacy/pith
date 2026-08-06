# numbers

pith has four numeric types. three of them are exact and one of them is not,
and knowing which is which is the whole of this page.

| type | holds | exact? | cost |
| --- | --- | --- | --- |
| `Int` | whole numbers, 64 bits, two's complement | yes, until it overflows | a machine register |
| `Float` | IEEE-754 binary double | no | a machine register |
| `std.bigint.BigInt` | whole numbers, any size | yes | a heap value; one machine word for anything under 10^18 |
| `std.decimal.Decimal` | decimal fractions, any size | yes | a heap value wrapping a BigInt |

The short version: use `Int` for counts, indices, ids, and bytes. Use `Float`
for measurements, ratios, and anything you are going to plot. Use `Decimal` for
money, tax, invoice lines, and anything a person will reconcile against a
statement. Use `BigInt` when a whole number outgrows 64 bits.

## why money is never a Float

A `Float` stores a binary fraction. `0.1` is not a binary fraction, so a Float
cannot hold it — it holds the nearest one it can, which is slightly off. The
error is invisible in one value and accumulates across many:

```pith
mut total := 0.0
mut count := 0
while count < 10:
    total = total + 0.1
    count = count + 1
print("{total}")          # 0.9999999999999999
```

The same loop with a `Decimal` gives `1.00`, because a decimal fraction is what
it stores.

The second failure is size. A `Float` has 53 bits of mantissa, so it stops
being able to name consecutive integers past 2^53. A ledger balance of
`12345678901234567.89` — well within a `NUMERIC(20,2)` column — comes back from
a Float as `12345678901234568`. The cents are gone and the integer part is off
by one, and nothing anywhere reports an error.

Neither failure announces itself. That is what makes it worth a type.

## std.decimal

A `Decimal` is an arbitrary-precision integer and a scale: the value is
`unscaled / 10^scale`. This is Java's `BigDecimal`, and it is also exactly what
SQL `NUMERIC(p, s)` means, which is why a database column round-trips through it
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

`1.10` parses to an unscaled 110 at scale 2 and prints back as `1.10`, not
`1.1`. That is deliberate: a `NUMERIC(10,2)` column stores the trailing zero and
prints it, so preserving it is what makes the round-trip byte-identical.

Comparison ignores the scale — `1.0` and `1.00` are equal — so the scale never
changes an answer, only how the value reads. `strip_trailing_zeros` drops it
when you want the shorter form.

Addition and subtraction take the wider of the two scales, so nothing is
dropped. Multiplication takes the sum of the two scales, because that is what an
exact product needs: `1.05 * 1.05` is `1.1025` at scale 4. Rescale it back when
you want it at scale 2, and say how it should round.

### there is no conversion from Float

On purpose. Java's `new BigDecimal(0.1)` gives
`0.1000000000000000055511151231257827021181583404541015625`, which is the honest
answer and almost never the wanted one; the alternative — taking the decimal the
Float was *printed* as and pretending it was exact — hides the error rather than
removing it. Neither belongs behind a function that looks safe.

If you genuinely have a `Float` and want a `Decimal`, format it and parse the
text, so the lossy step is the one you wrote:

```pith
approximate := decimal.parse(fmt_float(measurement, 4))!
```

`to_float` goes the other way and is documented as lossy, because printing and
plotting are real needs.

### division says how it rounds

Division is the only operation here that can be inexact — `1/3` has no decimal
form — so `div` takes the scale and the rounding mode at the call site. There is
no default, because a default is how a fraction of a cent goes missing:

```pith
share := decimal.div(total, decimal.from_int(3), 2, decimal.half_even())!
```

The seven modes are `down` (truncate toward zero), `up` (away from zero),
`floor` (toward negative infinity), `ceiling` (toward positive infinity),
`half_up`, `half_down`, and `half_even`.

**`half_even` is the one to use for money.** It sends an exact midpoint to
whichever neighbour ends in an even digit, so midpoints go up and down equally
often and a long run of transactions does not drift. `half_up` — the rounding
taught in school — biases every midpoint upward, which over a million rows is a
number someone will eventually notice.

When you expect a division to come out exactly, say so:

```pith
half := decimal.div_exact(amount, decimal.from_int(2))!
```

`div_exact` computes the quotient exactly and fails if it does not terminate, so
an assumption that stops holding shows up as an error rather than as a quiet
rounding.

### splitting an amount without losing a cent

Dividing `10.00` three ways and rounding each share gives `3.33` three times,
which is `9.99`. `allocate` hands the leftover minor units out one at a time so
the parts sum back to exactly the total:

```pith
shares := decimal.allocate(decimal.parse("10.00")!, [1, 1, 1])!
# 3.34, 3.33, 3.33
```

The weights are integer proportions, so `[3, 7]` splits 30/70.

## std.bigint

`BigInt` is the arbitrary-precision integer `Decimal` is built on, and it stands
alone for whole numbers that outgrow 64 bits.

```pith
import std.bigint as bigint

value := bigint.parse("170141183460469231731687303715884105727")!
print(bigint.to_string(bigint.mul(value, value)))
```

`add`, `sub`, and `mul` are exact. `divmod` truncates toward zero and gives the
remainder the sign of the dividend, matching pith's own `/` and `%` on `Int`,
and fails on a zero divisor rather than returning a fabricated value.

A magnitude of at most 18 decimal digits lives in a machine `Int` inside the
value, with no list and no loop, so ordinary-sized arithmetic does not pay for
the general path.

Two things it deliberately does not do:

- **no bitwise or shift operations.** The limb base is 10^9 rather than a power
  of two, which makes decimal conversion and scaling by a power of ten linear —
  the workload this type exists for — at the cost of making bit twiddling
  expensive. `std.bits` operates on `Int` and is the right tool for that.
- **not for cryptography.** Nothing here is constant-time and there is no
  modular exponentiation. Key material and signatures belong in `std.crypto`.

## numbers from a database

A `NUMERIC` (postgres) or `DECIMAL` (mysql) column decodes to
`std.sql.Value.Numeric`, carrying a `Decimal`. Read it with `row.decimal(name)`:

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
floats. Reading a `Numeric` column with `row.float(name)` still works and still
rounds — the coercion is there so existing code keeps compiling, not because it
is a good idea.

Bind a decimal back by its text, which is exact:

```pith
handle.exec("insert into ledger (amount) values ($1)", [decimal.to_string(amount)])!
```

A value the driver cannot parse as a number — postgres sends `NaN`, `Infinity`
and `-Infinity` for a numeric — decodes to `Value.Text` holding the original
string. It is never turned into a zero.

## limits

`Decimal` caps its scale at 16383, which is postgres's own ceiling on a
numeric's display scale, and `parse` refuses an exponent that would expand past
a million digits rather than letting untrusted input turn into an allocation.
`bigint.ten_pow` and `bigint.pow` refuse a result past 1.8 million digits for
the same reason. These are types for money and measurements, not for computing a
constant to a million places.
