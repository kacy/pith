# Passwords and tokens

Authentication in Pith is three modules. `std.crypto.password` handles the
password at rest, `std.crypto.jwt` handles the session that follows, and
`std.crypto.jwks` handles the tokens somebody else signed.
`examples/auth.pith` runs the first two end to end,
`examples/web_auth.pith` wires them into a `std.web` server (a login route
that issues tokens and a middleware that guards everything else), and
`examples/jwks_verify.pith` verifies against a key set an issuer publishes.

For a browser instead of an API, the session is the better carrier: a token
in a cookie cannot be revoked before it expires, and a server-side session
can. `std.web.session`, `std.web.csrf` and `std.web.auth` are that path.
`docs/web.md` covers them, and `examples/web_login.pith` runs a password
login through all three. This file covers only the three crypto modules
underneath.

## Hashing a password

`hash` takes a password and returns a PHC string. `verify` takes the password
and the stored string and says whether they match.

```pith
import std.crypto.password as password

stored := password.hash("correct horse battery staple")!
# $argon2id$v=19$m=19456,t=2,p=1$hMDYzMFA5NHRoZQ$Zk5MPFY7...

password.verify("correct horse battery staple", stored)  # true
password.verify("something else", stored)                # false
```

The salt is fresh OS randomness on every call, so hashing the same password
twice gives two different strings. The comparison at the end of `verify` goes
through `std.crypto.subtle`, so it looks at every byte whether or not the first
one matched. A comparison that returns early leaks how much of the tag was
right, one request at a time.

`verify` returns a `Bool` instead of a result. A hash it cannot parse is a
failed verification, which is the only safe reading: an API that returned an
error there would sooner or later be called by something that treated
"unparseable" as "fine".

## The parameters, and why these ones

The defaults are OWASP's Argon2id parameters: 19 MiB of memory, two passes, one
lane, a 16 byte salt and a 32 byte tag. That measures around 30 ms per hash on
a modest core.

| | Memory | Passes | Lanes | Measured |
| --- | --- | --- | --- | --- |
| The default | 19 MiB | 2 | 1 | ~30 ms |
| RFC 9106 first option | 2 GiB | 1 | 4 | Over the runtime's 1 GiB cap |
| RFC 9106 second option | 64 MiB | 3 | 4 | ~230 ms |

30 ms is a deliberate middle. It is expensive enough that an attacker who
steals the database is buying time by the CPU-year, and cheap enough that a
login endpoint under load is not attacking itself. An auth path that spends a
quarter of a second per attempt hands anyone with a script a way to saturate
it. The runtime derives single-threaded, so raising `lanes` costs wall clock
time instead of saving it.

To move off the defaults, build a `Params` and use `hash_with`:

```pith
strict := password.params(65536, 3, 1)   # memory kib, passes, lanes
stored := password.hash_with(secret, strict)!
```

`params_with_lengths` also sets the salt and tag sizes. Everything is range
checked before any derivation happens: at least 8 KiB of memory per lane, a
salt of at least 8 bytes, a tag of at least 16, and no more than 1 GiB of
memory, which is the cap the runtime enforces.

## Raising the parameters later

The cost parameters are written into the PHC string, so a hash stored under old
parameters stays verifiable after you raise them. `needs_rehash` tells you
which ones to replace:

```pith
wanted := password.params(65536, 3, 1)

if password.verify(attempt, stored):
    if password.needs_rehash(stored, wanted):
        stored = password.hash_with(attempt, wanted)!
        save(user, stored)
```

The rehash happens inside the successful login, because that is the only moment
the plaintext password is in hand. A hash that cannot be parsed at all also
reports as needing a rehash, which silently migrates anything left over from an
older scheme the first time its owner signs in.

## JSON Web Tokens

`std.crypto.jwt` reads and writes the JWS compact serialization. Every
algorithm both signs and verifies: HS256, HS384 and HS512 take a shared
secret, and PS256, RS256, ES256 and EdDSA take a PKCS#8 private key (the DER
inside what `openssl genpkey` writes, once the PEM armor is stripped).

Which one to pick: HS\* when a single service both issues and checks its own
tokens; EdDSA or PS256 when other parties need to verify without being able to
forge; RS256 when the other end demands it, which hosted issuers commonly do.
ES256 is there for the ecosystems that standardized on P-256.

```pith
import std.bytes as bytes
import std.crypto.jwt as jwt

secret := bytes.from_string_utf8(env("SESSION_SECRET"))

token := jwt.claims().issuer("chat").subject("u-1024").expires_in(3600).sign_hs256(secret)!
session := jwt.verify_hs256(token, secret, jwt.default_options())!
print(session.subject())
```

The builder writes the JSON, escapes the values, and stamps `iat` and `exp`
at signing time. `expires_in` counts seconds from now, so no epoch arithmetic
reaches your code. Custom claims go on with `.claim("role",
"admin")` and `.claim_int("level", 9)`; a custom claim that spells a
registered name (`iss`, `exp`, ...) is refused instead of silently
overriding the setters. The `sign_*` methods cover every algorithm the
module signs with.

`verify_*` hands back a `Verified`. Read the registered subject with
`.subject()` and anything else with `.claim(name)` / `.claim_int(name)`.
For a struct of your own, the claims JSON is still there:
`json.decode_text[T]` on `.claims` fills it. Raw claims JSON can also be
signed directly with `jwt.sign_hs256(text, secret)` when it comes from
somewhere else; a Pith string literal writes JSON with doubled braces,
since a single `{` starts an interpolation.

## The two things JWT libraries get wrong

### Algorithm confusion

Every verifier names the algorithm it accepts. `verify_hs256` verifies HS256
and nothing else; the token's own `alg` header is only ever compared against
the name the caller passed, and no code path selects a verifier from it.

This closes the classic attack: take a token meant for an RSA verifier,
re-sign it as HS256 using the RSA *public* key as the HMAC secret, and hand it
to a verifier that trusts the header. Against `verify_rs256` that token is
refused on its `alg` before its signature is looked at.

### alg: none

Such a token is refused outright, in any spelling of case, with or without a
signature field bolted on the end, and there is no option to turn it back on.
The spec may describe an unsecured JWS, but a verifier should never accept
one.

### And the smaller ones

A `crit` header parameter is rejected per RFC 7515 section 4.1.11, since a
verifier that ignores an extension the issuer marked critical is ignoring the
thing the issuer said not to ignore. Signature comparison goes through
`std.crypto.subtle`. The token length, header parameter count, claim count and
audience list are all bounded.

## Checking the claims

`Options` decides which registered claims get checked. The defaults check
`exp`, `nbf` and `iat` with a minute of clock skew, and compare nothing the
caller has not named:

```pith
expected := jwt.with_audience(jwt.with_issuer(jwt.default_options(), "chat"), "web")
session := jwt.verify_hs256(token, secret, expected)!
```

The builders compose, each returning a new `Options`:

- `with_issuer`, `with_audience`, `with_subject`: compare `iss`, `sub`, and
  `aud`, which may be a string or an array of them
- `with_leeway`: how much clock skew the time comparisons tolerate
- `requiring_exp`: refuse a token that carries no `exp` at all
- `at_time`: validate against a fixed Unix timestamp instead of the clock
- `without_time_checks`: turn off `exp`, `nbf` and `iat`, for replaying a
  fixed vector and not for traffic

## Tokens you did not mint

`jwt.verify_*` wants the key in hand. An identity provider does not hand you
one: it publishes a JSON Web Key Set at a URL and rotates what is in it when
it likes. `std.crypto.jwks` handles that side.

```pith
import std.crypto.jwks as jwks

keys := jwks.cache("https://issuer.example.com/.well-known/jwks.json")
expected := jwt.with_issuer(jwt.default_options(), "https://issuer.example.com")
session := keys.verify(token, expected)!
```

Build the cache once at startup and share it. It fetches on first use and
again when its TTL has passed (`.ttl(seconds)`, an hour by default), so an
issuer is asked for its keys on a timer instead of on a request. The fetch
has a timeout (`.timeout_ms`) and a response size cap (`.max_bytes`), and the
lock is never held across it.

With a document you already have (vendored, or read from disk at startup),
skip the cache:

```pith
set := jwks.parse(document)!
session := set.verify(token, expected)!
```

`set.keys` is the parsed list. Each `Key` carries its `kid`, the algorithm it
verifies, and the `material` in the form the matching `jwt.verify_*` takes:
the PKCS#1 RSAPublicKey DER for RS256 and PS256, the uncompressed point for
ES256, the raw 32 bytes for EdDSA. A caller who wants to do the verification
itself can, without redoing the JWK conversion.

### The algorithm comes from the key

Which algorithm a key verifies is a property of the key: an RSA key verifies
RS256 (or PS256 if it says so), a P-256 key verifies ES256, an Ed25519 key
verifies EdDSA. `jwks` reads that off the key material (the JWK's own `alg`
when it states one, `kty` and `crv` otherwise) and hands the answer to the
matching `jwt.verify_*`, which then compares the token's `alg` header against
that name and refuses the token if they differ.

So the token's header selects nothing. Its `kid` picks which key to try, and
that is all it does. This is the same shape `std.crypto.jwt` already has,
extended one step out, so a rotated key set cannot introduce an algorithm
either.

A symmetric key in a key set (`kty: oct`) is refused outright, and the whole
parse fails instead of skipping it. A key set is a public document; a shared
secret in one hands every reader the ability to sign. A JWK that claims an
algorithm its key type cannot verify (`kty: RSA` with `alg: ES256`) is
refused for the same reason: it describes something that does not exist,
and the interesting question is who wrote it.

Keys of a type this module does not implement, and keys marked `use: enc`,
are skipped instead of being fatal: an issuer publishing one key you can use
and one you cannot is publishing a set you can still use.

### Rotation

A token naming a `kid` the cached set does not hold is what a rotation looks
like from here, so it is worth one refetch, but no more than one per cooldown
(`.refresh_cooldown(seconds)`, a minute by default). Otherwise a stream of
invented `kid`s becomes a stream of requests at the issuer. `refresh()`
fetches now, whatever the TTL says, for when something outside told you the
keys moved.

A token with no `kid` is tried against every key in the set, which is what a
single-key issuer needs; a set with exactly one key reports that key's own
refusal instead of a generic one, so an expired token still says "expired".

### HTTPS

The key set is the root of trust for every token you accept. Over plain HTTP
anyone on the path replaces it with keys of their own and mints whatever they
like. `jwks.cache` allows an `http` URL for a key set served by a sidecar on
loopback, and refuses any other scheme; everything else should be `https`.

`examples/jwks_verify.pith` runs the whole thing against an issuer in the
same process: the published set, a token it signed, and the forgeries that
get turned away.

## Interoperability

The tests verify the RFC 7515 appendix A.1 HS256 token and the appendix A.3
ES256 token against the exact serializations the RFC prints, and
`tests/cases/test_jwt_asymmetric.pith` verifies an RS256 and an EdDSA token
signed with OpenSSL. A token that only round trips against the module that
made it says nothing about talking to anyone else.

The signing half is held to the same standard. RSA PKCS#1 v1.5 and Ed25519 are
deterministic, so for a fixed key and claims there is exactly one valid token,
and the tests check that `sign_rs256` and `sign_eddsa` produce, byte for byte,
the token OpenSSL produces. ES256 cannot be pinned like that (every signature
uses a fresh nonce on purpose, because a repeated ECDSA nonce forfeits the
private key), so it is tested by round trip and by checking that two
signatures over the same input differ.

ES256 needs a small translation on the way in: JWS carries the ECDSA signature
as r and s glued together at a fixed width, and the runtime's verifier reads
the ASN.1 DER encoding, so `verify_es256` re-wraps it. Signing needs no
translation, because the runtime signs in the fixed-width form directly.

Key formats, which are the usual place to get stuck:

- `verify_eddsa` takes the raw 32 byte Ed25519 public key
- `verify_es256` takes the uncompressed P-256 point: `0x04`, then x, then y
- `verify_rs256` and `verify_ps256` take the PKCS#1 RSAPublicKey DER, which is
  what `std.crypto.x509` returns as a certificate's `subject_public_key`
- Every `sign_*` takes a PKCS#8 private key, which is what
  `encoding.pem_decode` gives you from a `BEGIN PRIVATE KEY` file. An RSA key
  in the older `BEGIN RSA PRIVATE KEY` form needs one pass through
  `openssl pkcs8 -topk8` first
