# passwords and tokens

authentication in pith is two modules. `std.crypto.password` handles the
password at rest, and `std.crypto.jwt` handles the session that follows.
`examples/auth.pith` runs the whole flow end to end.

## hashing a password

`hash` takes a password and returns a phc string. `verify` takes the password
and the stored string and says whether they match.

```pith
import std.crypto.password as password

stored := password.hash("correct horse battery staple")!
# $argon2id$v=19$m=19456,t=2,p=1$hMDYzMFA5NHRoZQ$Zk5MPFY7...

password.verify("correct horse battery staple", stored)  # true
password.verify("something else", stored)                # false
```

the salt is fresh os randomness on every call, so hashing the same password
twice gives two different strings. the comparison at the end of `verify` goes
through `std.crypto.subtle`, so it looks at every byte whether or not the first
one matched — a comparison that returns early leaks how much of the tag was
right, one request at a time.

`verify` returns a `Bool` rather than a result. a hash it cannot parse is a
failed verification, which is the only safe reading: an api that returned an
error there would sooner or later be called by something that treated
"unparseable" as "fine".

## the parameters, and why these ones

the defaults are owasp's argon2id parameters: 19 mib of memory, two passes, one
lane, a 16 byte salt and a 32 byte tag. that measures around 30 ms per hash on
a modest core.

| | memory | passes | lanes | measured |
| --- | --- | --- | --- | --- |
| the default | 19 mib | 2 | 1 | ~30 ms |
| rfc 9106 first option | 2 gib | 1 | 4 | over the runtime's 1 gib cap |
| rfc 9106 second option | 64 mib | 3 | 4 | ~230 ms |

30 ms is a deliberate middle. it is expensive enough that an attacker who
steals the database is buying time by the cpu-year, and cheap enough that a
login endpoint under load is not attacking itself — an auth path that spends a
quarter of a second per attempt hands anyone with a script a way to saturate
it. the runtime derives single-threaded, so raising `lanes` costs wall clock
rather than saving it.

to move off the defaults, build a `Params` and use `hash_with`:

```pith
strict := password.params(65536, 3, 1)   # memory kib, passes, lanes
stored := password.hash_with(secret, strict)!
```

`params_with_lengths` also sets the salt and tag sizes. everything is range
checked before any derivation happens: at least 8 kib of memory per lane, a
salt of at least 8 bytes, a tag of at least 16, and no more than 1 gib of
memory, which is the cap the runtime enforces.

## raising the parameters later

the cost parameters are written into the phc string, so a hash stored under old
parameters stays verifiable after you raise them. `needs_rehash` is what tells
you which ones to replace:

```pith
wanted := password.params(65536, 3, 1)

if password.verify(attempt, stored):
    if password.needs_rehash(stored, wanted):
        stored = password.hash_with(attempt, wanted)!
        save(user, stored)
```

the rehash happens inside the successful login, because that is the only moment
the plaintext password is in hand. a hash that cannot be parsed at all also
reports as needing a rehash, which quietly migrates anything left over from an
older scheme the first time its owner signs in.

## json web tokens

`std.crypto.jwt` reads and writes the jws compact serialization. every
algorithm both signs and verifies: HS256, HS384 and HS512 take a shared
secret, and PS256, RS256, ES256 and EdDSA take a pkcs#8 private key — the der
inside what `openssl genpkey` writes, once the pem armor is stripped.

which one to pick: HS\* when a single service both issues and checks its own
tokens; EdDSA or PS256 when other parties need to verify without being able to
forge; RS256 when the other end demands it, which hosted issuers commonly do.
ES256 is there for the ecosystems that standardized on p-256.

```pith
import std.bytes as bytes
import std.crypto.jwt as jwt

secret := bytes.from_string_utf8(env("SESSION_SECRET"))
claims := "{{\"iss\":\"chat\",\"sub\":\"u-1024\",\"exp\":1750003600}}"

token := jwt.sign_hs256(claims, secret)!
session := jwt.verify_hs256(token, secret, jwt.default_options())!
print(session.claims)
```

json in a pith string literal is written with doubled braces, since a single
`{` starts an interpolation.

`verify_*` hands back a `Verified` holding the header and claims as the json
text they arrived as. parse them with `std.json` — `json.decode_text[T]` if you
have a struct for your claims, `json.parse` if you would rather have handles.

## the two things jwt libraries get wrong

### algorithm confusion

every verifier names the algorithm it accepts. `verify_hs256` verifies HS256
and nothing else; the token's own `alg` header is only ever compared against
the name the caller passed, and no code path selects a verifier from it.

the attack this closes is the classic one. take a token meant for an rsa
verifier, re-sign it as HS256 using the rsa *public* key as the hmac secret,
and hand it to a verifier that trusts the header. against `verify_rs256` that
token is refused on its `alg` before its signature is looked at.

### alg: none

refused outright, with no option to turn it back on, spelled in any case, with
or without a signature field bolted on the end. an unsecured jws is a valid
thing for the spec to describe and never a valid thing for a verifier to
accept.

### and the smaller ones

a `crit` header parameter is rejected per rfc 7515 section 4.1.11, since a
verifier that ignores an extension the issuer marked critical is ignoring the
thing the issuer said not to ignore. signature comparison goes through
`std.crypto.subtle`. the token length, header parameter count, claim count and
audience list are all bounded.

## checking the claims

`Options` decides which registered claims get checked. the defaults check
`exp`, `nbf` and `iat` with a minute of clock skew, and compare nothing the
caller has not named:

```pith
expected := jwt.with_audience(jwt.with_issuer(jwt.default_options(), "chat"), "web")
session := jwt.verify_hs256(token, secret, expected)!
```

the builders compose, each returning a new `Options`:

- `with_issuer`, `with_audience`, `with_subject` — compare `iss`, `sub`, and
  `aud`, which may be a string or an array of them
- `with_leeway` — how much clock skew the time comparisons tolerate
- `requiring_exp` — refuse a token that carries no `exp` at all
- `at_time` — validate against a fixed unix timestamp rather than the clock
- `without_time_checks` — turn off `exp`, `nbf` and `iat`, for replaying a
  fixed vector and not for traffic

## interoperability

the tests verify the rfc 7515 appendix a.1 HS256 token and the appendix a.3
ES256 token against the exact serializations the rfc prints, and
`tests/cases/test_jwt_asymmetric.pith` verifies an RS256 and an EdDSA token
signed with openssl. a token that only round trips against the module that made
it says nothing about talking to anyone else.

the signing half is held to the same standard. rsa pkcs#1 v1.5 and ed25519 are
deterministic, so for a fixed key and claims there is exactly one valid token —
and the tests check that `sign_rs256` and `sign_eddsa` produce, byte for byte,
the token openssl produces. ES256 cannot be pinned like that (every signature
uses a fresh nonce, on purpose — a repeated ecdsa nonce forfeits the private
key), so it is tested by round trip and by checking two signatures over the
same input differ.

ES256 needs a small translation on the way in: jws carries the ecdsa signature
as r and s glued together at a fixed width, and the runtime's verifier reads
the asn.1 der encoding, so `verify_es256` re-wraps it. signing has no
translation — the runtime signs in the fixed-width form directly.

key formats, which are the usual place to get stuck:

- `verify_eddsa` takes the raw 32 byte ed25519 public key
- `verify_es256` takes the uncompressed p-256 point: `0x04`, then x, then y
- `verify_rs256` and `verify_ps256` take the pkcs#1 rsapublickey der, which is
  what `std.crypto.x509` returns as a certificate's `subject_public_key`
- every `sign_*` takes a pkcs#8 private key, which is what
  `encoding.pem_decode` gives you from a `BEGIN PRIVATE KEY` file. an rsa key
  in the older `BEGIN RSA PRIVATE KEY` form needs one pass through
  `openssl pkcs8 -topk8` first
