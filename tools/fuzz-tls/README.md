# tls/x509 parser fuzzing

`fuzz_tls.pith` feeds mutated real inputs to every attacker-reachable parser in
the TLS and X.509 stack — certificate DER and PEM, ClientHello/ServerHello,
HelloRetryRequest, EncryptedExtensions, NewSessionTicket, CertificateRequest,
alerts, and the record layer — and holds them to two oracles:

1. no call crashes the process or hangs; `ok` and `err` are both answers, a
   signal or a timeout is a bug.
2. a parser that accepted a mutation must return a value the accessors can walk
   without dying.

The corpus is built from the repo's certificate fixtures and from the stack's
own encoders, so mutations start from structurally valid inputs and drift.

```
pith build tools/fuzz-tls/fuzz_tls.pith
./tools/fuzz-tls/fuzz_tls --count 3000 --seed 7     # explore
```

It is deterministic per seed. `make fuzz-check` runs a fixed slice in CI; pass
`--seed` to explore new ground locally.
