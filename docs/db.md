# db

`std.db` is a small, opinionated layer over pith's pure-pith postgres and mysql
drivers. you open a pooled connection from a url and run explicit sql. it is not
an orm: you write the queries, and the layer handles pooling, parameters, and
typed reads.

each driver has its own entry module — `std.db.postgres` and `std.db.mysql` —
because a single program links one driver at a time. pick the one that matches
your database and import it as `db`; the surface is the same either way.

## a query in ten lines

```pith
import std.db.postgres as db
import std.sql as sql

fn main() -> Int!:
    handle := db.open("postgres://me:pw@127.0.0.1:5432/app")!
    for row in handle.query("select id, name from users where id = $1", ["7"])!:
        id := row.int("id")
        name := row.text("name")
        print("{id} {name}")
    handle.close()
    return 0
```

`open` parses the url, picks a default pool size, and returns a `Db` handle.
connections open lazily, on the first query. the handle is cheap to copy and
safe to share across tasks, so a server opens one at startup and hands it to
every request.

## connection urls

```
postgres://user:password@host:port/dbname?pool=8
mysql://user:password@host:port/dbname
```

the scheme selects the driver: `postgres` (or `postgresql`) and `mysql`. the
port defaults per driver — 5432 for postgres, 3306 for mysql — and the user,
password, and `?pool=N` size are all optional. `pool` sets how many idle
connections the pool keeps for reuse; it defaults to 8.

`std.db.parse_url` exposes the parsed parts as a `Dsn` if you want them directly.

## queries and parameters

`query` runs a select and returns the rows as a `List[std.sql.Row]`. iterate it,
index it, or ask its length:

```pith
rows := handle.query("select id, name from users", [])!
print("{rows.len()} users")
```

parameters keep values off the sql string. the placeholder syntax follows the
driver: `$1`, `$2`, ... for postgres and `?` for mysql.

```pith
# postgres
handle.query("select name from users where id = $1", ["7"])!

# mysql
handle.query("select name from users where id = ?", ["7"])!
```

for a query that returns at most one row, `std.db.first` gives you the first row
or `none`:

```pith
import std.db as dbc

found := dbc.first(handle.query("select name from users where id = $1", ["7"])!)
if found != none:
    print(found.value().text("name"))
```

## typed column reads

every row is a `std.sql.Row`. read a column by name or by index, and pick the
type you expect:

```pith
row.int("id")        # Int, by name
row.text("name")     # String
row.float("score")   # Float
row.decimal("amount")# std.decimal.Decimal, exact
row.bool("active")   # Bool

row.int_at(0)        # the same, by index
row.text_at(1)
```

an unknown column or a type mismatch reads as the zero value (`0`, `""`,
`false`), which keeps the common path terse. for a column that may be null, the
`opt_` accessors return an optional instead:

```pith
score := row.opt_float("score")
if score == none:
    print("no score")
else:
    print("{score.value()}")
```

`opt_int`, `opt_float`, `opt_decimal`, `opt_text`, and `opt_bool` all return
`none` for a null or missing column, with `_at` variants for positional access.

## exact numbers

a `NUMERIC` (postgres) or `DECIMAL` (mysql) column is exact by definition, and
it decodes to an exact `std.decimal.Decimal` rather than to a `Float`. read it
with `row.decimal(name)`:

```pith
import std.decimal as decimal

mut balance := decimal.zero()
for row in handle.query("select amount from ledger", [])!:
    balance = decimal.add(balance, row.decimal("amount"))
print(decimal.to_string(balance))
```

**do not read a numeric column with `row.float`.** a `Float` cannot hold most
decimal fractions and runs out of integers past 2^53, so a `NUMERIC(20,2)`
balance of `12345678901234567.89` arrives as `12345678901234568` — cents gone,
integer part off by one, and no error anywhere. the coercion still exists so
code written before `Numeric` did keeps compiling; it is not a good idea.

`float4` and `float8` still decode to `Value.Real`, because those genuinely are
floats.

write a decimal back by binding its text, which is exact:

```pith
handle.exec("insert into ledger (amount) values ($1)", [decimal.to_string(amount)])!
```

a value the driver cannot parse as a number — postgres sends `NaN`, `Infinity`
and `-Infinity` for a numeric — comes back as `Value.Text` holding the original
string, never as a fabricated zero.

see [docs/numbers.md](numbers.md) for when to reach for `Int`, `Float`,
`Decimal`, and `BigInt`, and for the rounding modes.

## writes

`exec` runs an insert, update, delete, or ddl statement and returns an
`ExecResult`:

```pith
result := handle.exec("insert into users (name) values (?)", ["ada"])!
print("{result.rows_affected} rows, id {result.last_insert_id}")
```

`rows_affected` is how many rows the statement changed. `last_insert_id` is the
id an auto_increment column produced — mysql reports it directly; postgres does
not, so it is always 0 there. to read a generated id on postgres, add
`returning id` to the insert and read it with `query`.

## transactions

when a group of statements has to succeed or fail as a unit, open a transaction.
`begin` pins one connection from the pool and sends `begin`; every statement you
run on the returned `Tx` uses that same connection. finish with `commit` to keep
the changes or `rollback` to throw them away — either one returns the connection
to the pool.

```pith
t := handle.begin()!
t.exec("update accounts set balance = balance - $1 where id = $2", ["100", "1"])!
t.exec("update accounts set balance = balance + $1 where id = $2", ["100", "2"])!
t.commit()!
```

the important case is the one where something goes wrong partway through: the
transaction must roll back so a half-applied change never lands. reach for
`errdefer` — it runs only when the function leaves through an error, which is
exactly the rollback case. schedule it right after `begin`, then write the body
straight through:

```pith
fn transfer(handle: db.Db, src: String, dst: String, cents: String) -> Bool!:
    t := handle.begin()!
    errdefer t.rollback()       # only if a `!` below propagates

    t.exec("update accounts set balance = balance - $1 where id = $2", [cents, src])!
    t.exec("update accounts set balance = balance + $1 where id = $2", [cents, dst])!
    t.commit()!
    return true
```

if either write fails, its `!` propagates out of `transfer` and the `errdefer`
rolls the transaction back on the way. if both land and the `commit` succeeds,
the function returns normally and the `errdefer` stays quiet — no rollback after
a good commit. a plain `defer t.rollback()` would fire on every exit and undo the
commit you just made. see [defer.md](defer.md) for the full rules.

`rollback` never fails, so it drops straight onto the error path. it is also
safe after a statement has already failed: such a statement leaves its connection
out of protocol sync, so `rollback` discards the connection instead of returning
it to the pool, and a `commit` on that transaction fails rather than pretending
to succeed.

`Tx` exposes the same `query` and `exec` as `Db`, so typed reads and
`ExecResult` writes work exactly as they do outside a transaction.

## prepared statements

for a query you run many times with different parameters, prepare it once.
`prepare` pins a connection and parses the statement on it; each `query` or
`exec` on the returned `Stmt` binds fresh parameters and reuses that parse.
`close` frees the statement and returns the connection to the pool.

```pith
stmt := handle.prepare("select name from users where id = $1")!
for id in ["1", "2", "3"]:
    for row in stmt.query([id])!:
        print(row.text("name"))
stmt.close()
```

a `Stmt` holds its connection for its whole lifetime, so close it when you are
done — a long-lived one keeps a connection out of the pool. reach for a prepared
statement on a hot path; plain `query`/`exec` already bind parameters safely for
everything else.

## pooling and concurrency

pooling is transparent. every `query` and `exec` borrows a connection from the
pool and returns it when the call finishes, so the tcp and auth handshake happen
once per connection rather than once per query. a query that fails leaves its
connection out of protocol sync, so that connection is closed rather than
returned to the pool.

the pool is guarded by a mutex, so many green tasks can share one `Db` and call
`query`/`exec` concurrently. `close` drains the pool, closing every idle
connection.

## observability

`query` and `exec` are instrumented in the box, the same as the http and grpc
clients. each call increments a `db_queries_total` counter (labeled by driver,
op, and ok/err status) and records a `db_query_duration_ms` histogram, and each
opens a client trace span. scrape them through `std.metrics` and `std.trace`
without wiring anything up.

## a note on drivers

the drivers themselves — `std.postgres` and `std.mysql` — remain available for
lower-level work: raw connections, prepared statements you manage by hand, and
the simple-vs-extended protocol choice. `std.db` is the path most applications
want; reach for the driver directly when you need the control.
