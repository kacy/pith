# DB

`std.db` is a small, opinionated layer over Pith's pure-Pith Postgres and
MySQL drivers. You open a pooled connection from a URL and run explicit SQL. It
is not an ORM: you write the queries, and the layer handles pooling,
parameters, and typed reads.

Each driver has its own entry module (`std.db.postgres` and `std.db.mysql`)
because a single program links one driver at a time. Pick the one that matches
your database and import it as `db`; the surface is the same either way.

## A query in ten lines

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

`open` parses the URL, picks a default pool size, and returns a `Db` handle.
Connections open lazily, on the first query. The handle is cheap to copy and
safe to share across tasks, so a server opens one at startup and hands it to
every request.

## Connection URLs

```
postgres://user:password@host:port/dbname?pool=8
mysql://user:password@host:port/dbname
```

The scheme selects the driver: `postgres` (or `postgresql`) and `mysql`. The
port defaults per driver (5432 for Postgres, 3306 for MySQL), and the user,
password, and `?pool=N` size are all optional. `pool` caps how many connections
the pool may have open at once; it defaults to 8. Once the pool is at the cap, a
query waits for a connection to come back instead of opening another.

`std.db.parse_url` exposes the parsed parts as a `Dsn` if you want them directly.

## Queries and parameters

`query` runs a select and returns the rows as a `List[std.sql.Row]`. Iterate it,
index it, or ask its length:

```pith
rows := handle.query("select id, name from users", [])!
print("{rows.len()} users")
```

Parameters keep values off the SQL string. The placeholder syntax follows the
driver: `$1`, `$2`, ... for Postgres and `?` for MySQL.

```pith
# postgres
handle.query("select name from users where id = $1", ["7"])!

# mysql
handle.query("select name from users where id = ?", ["7"])!
```

For a query that returns at most one row, `std.db.first` gives you the first row
or `none`:

```pith
import std.db as dbc

found := dbc.first(handle.query("select name from users where id = $1", ["7"])!)
if found != none:
    print(found.value().text("name"))
```

## Typed column reads

Every row is a `std.sql.Row`. Read a column by name or by index, and pick the
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

An unknown column or a type mismatch reads as the zero value (`0`, `""`,
`false`), which keeps the common path terse. For a column that may be null, the
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

## Exact numbers

A `NUMERIC` (Postgres) or `DECIMAL` (MySQL) column is exact by definition, and
it decodes to an exact `std.decimal.Decimal` instead of to a `Float`. Read it
with `row.decimal(name)`:

```pith
import std.decimal as decimal

mut balance := decimal.zero()
for row in handle.query("select amount from ledger", [])!:
    balance = decimal.add(balance, row.decimal("amount"))
print(decimal.to_string(balance))
```

**Do not read a numeric column with `row.float`.** A `Float` cannot hold most
decimal fractions and runs out of integers past 2^53, so a `NUMERIC(20,2)`
balance of `12345678901234567.89` arrives as `12345678901234568`: the cents are
gone, the integer part is off by one, and there is no error anywhere. The
coercion still exists so that code written before `Numeric` did keeps
compiling, but using it is not a good idea.

`float4` and `float8` still decode to `Value.Real`, because those really are
floats.

Write a decimal back by binding its text, which is exact:

```pith
handle.exec("insert into ledger (amount) values ($1)", [decimal.to_string(amount)])!
```

A value the driver cannot parse as a number (Postgres sends `NaN`, `Infinity`
and `-Infinity` for a numeric) comes back as `Value.Text` holding the original
string, never as a fabricated zero.

See [docs/numbers.md](numbers.md) for when to reach for `Int`, `Float`,
`Decimal`, and `BigInt`, and for the rounding modes.

## Writes

`exec` runs an insert, update, delete, or DDL statement and returns an
`ExecResult`:

```pith
result := handle.exec("insert into users (name) values (?)", ["ada"])!
print("{result.rows_affected} rows, id {result.last_insert_id}")
```

`rows_affected` is how many rows the statement changed. `last_insert_id` is the
ID an auto_increment column produced. MySQL reports it directly; Postgres does
not, so it is always 0 there. To read a generated ID on Postgres, add
`returning id` to the insert and read it with `query`.

## Transactions

When a group of statements has to succeed or fail as a unit, open a transaction.
`begin` pins one connection from the pool and sends `begin`; every statement you
run on the returned `Tx` uses that same connection. Finish with `commit` to keep
the changes or `rollback` to throw them away. Either one returns the connection
to the pool.

```pith
t := handle.begin()!
t.exec("update accounts set balance = balance - $1 where id = $2", ["100", "1"])!
t.exec("update accounts set balance = balance + $1 where id = $2", ["100", "2"])!
t.commit()!
```

The important case is the one where something goes wrong partway through: the
transaction must roll back so a half-applied change never lands. Use
`errdefer`. It runs only when the function leaves through an error, which is
exactly the rollback case. Schedule it right after `begin`, then write the body
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

If either write fails, its `!` propagates out of `transfer` and the `errdefer`
rolls the transaction back on the way. If both land and the `commit` succeeds,
the function returns normally and the `errdefer` does not run, so there is no
rollback after a good commit. A plain `defer t.rollback()` would fire on every
exit and undo the commit you just made. See [defer.md](defer.md) for the full
rules.

`rollback` never fails, so it drops straight onto the error path. It is also
safe after a statement has already failed: such a statement leaves its connection
out of protocol sync, so `rollback` discards the connection instead of returning
it to the pool, and a `commit` on that transaction fails instead of pretending
to succeed.

A `Tx` that goes away without `commit` or `rollback` is rolled back by its
destructor. The destructor cannot send `rollback`, because it must not do
network I/O, so it closes the pinned connection instead and returns its slot to
the pool. The server rolls back a transaction whose client has gone. This is a
backstop: the connection is lost instead of reused, so finish a transaction
explicitly on every path you can.

`Tx` exposes the same `query` and `exec` as `Db`, so typed reads and
`ExecResult` writes work exactly as they do outside a transaction.

## Prepared statements

For a query you run many times with different parameters, prepare it once.
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

A `Stmt` holds its connection for its whole lifetime, so close it when you are
done; a long-lived one keeps a connection out of the pool. A `Stmt` that goes
away without `close` has its connection closed by its destructor, which frees
the statement on the server and returns the slot, but loses the connection.
Use a prepared statement on a hot path; plain `query`/`exec` already bind
parameters safely for everything else.

## Pooling and concurrency

Pooling is transparent. Every `query` and `exec` borrows a connection from the
pool and returns it when the call finishes, so the TCP and auth handshake happen
once per connection instead of once per query. A query that fails leaves its
connection out of protocol sync, so that connection is closed instead of
returned to the pool.

The pool is guarded by a mutex, so many green tasks can share one `Db` and call
`query`/`exec` concurrently. At most `pool` of them hold a connection at a time;
the rest wait their turn. `close` drains the pool, closing every idle
connection.

A `Db` that is never closed still gives its sockets back. When the last copy
of it goes (and every `Tx` and `Stmt` opened from it), the pool goes too, and
each idle connection closes its socket from its own destructor. The same holds
for a connection from `std.postgres`, `std.mysql` or `std.redis` used directly:
dropping it without `close` closes its socket. A destructor never writes to the
socket, so a TLS connection closed that way sends no close_notify; call `close`
for a clean shutdown.

## Observability

`query` and `exec` are instrumented out of the box, the same as the HTTP and
gRPC clients. Each call increments a `db_queries_total` counter (labeled by
driver, op, and ok/err status) and records a `db_query_duration_ms` histogram,
and each opens a client trace span. Scrape them through `std.metrics` and
`std.trace` without wiring anything up.

## A note on drivers

The drivers themselves (`std.postgres` and `std.mysql`) remain available for
lower-level work: raw connections, prepared statements you manage by hand, and
the simple-vs-extended protocol choice. TLS to the database also lives in the
drivers: `std.postgres.connect_tls` and `std.mysql.connect_tls` take a
`tls.Config` and a server name, and `std.db` has no TLS path of its own.
`std.db` is the path most applications want; use the driver directly when you
need an encrypted connection or the extra control.
