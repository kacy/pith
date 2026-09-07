#!/usr/bin/env bash
#
# regenerate the client-certificate fixtures used by the mutual-tls tests:
# tests/cases/test_tls12_client_auth.pith, tests/live/test_tls_client_auth_openssl_live.pith,
# tests/leaks/leak_tls12_client_auth.pith and examples/mutual_tls.pith.
#
# the set is one trusted client ca with three leaves under it (rsa, ecdsa, and
# an expired rsa one), plus a second ca that nothing trusts and a leaf under
# that. together they cover the answers a server must give: accept, refuse an
# untrusted issuer, refuse an expired certificate.
#
# the ca private keys are deliberately not committed. nothing needs them once
# the leaves are signed, and regenerating the set means running this script,
# which mints new keys for everything at once.
#
# needs openssl 3.5 or newer for the -not_before / -not_after flags the expired
# leaf is minted with. nothing else here is version sensitive, and ci never runs
# this script — it consumes the committed output.
#
# run from the repository root:
#   bash tests/live/fixtures/gen_client_cert_fixtures.sh
set -euo pipefail

out="tests/live/fixtures"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# ten years, so a checkout does not start failing on a date. the expired leaf
# below is the one certificate whose window is deliberately in the past.
days=3650

leaf_ext="$work/leaf.ext"
cat > "$leaf_ext" <<'EXT'
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature
extendedKeyUsage = clientAuth
EXT

ca_ext="$work/ca.ext"
cat > "$ca_ext" <<'EXT'
basicConstraints = critical, CA:TRUE
keyUsage = critical, keyCertSign, cRLSign
EXT

# make_ca <name> <common-name>
make_ca() {
    openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$work/$1.key" 2>/dev/null
    openssl req -x509 -new -key "$work/$1.key" -sha256 -days "$days" \
        -subj "/CN=$2" -extensions v3_ca -config <(cat <<CFG
[req]
distinguished_name = dn
[dn]
[v3_ca]
basicConstraints = critical, CA:TRUE
keyUsage = critical, keyCertSign, cRLSign
CFG
) -out "$work/$1.crt"
}

# make_leaf <name> <common-name> <ca-name> <extra x509 args...>
make_leaf() {
    local name="$1" cn="$2" ca="$3"
    shift 3
    openssl genpkey -algorithm "${KEY_ALG:-RSA}" ${KEY_OPT:-} -out "$work/$name.key" 2>/dev/null
    openssl req -new -key "$work/$name.key" -subj "/CN=$cn" -out "$work/$name.csr"
    openssl x509 -req -in "$work/$name.csr" -CA "$work/$ca.crt" -CAkey "$work/$ca.key" \
        -CAcreateserial -sha256 -extfile "$leaf_ext" -out "$work/$name.crt" "$@" 2>/dev/null
}

make_ca client-ca "pith-client-ca"
make_ca rogue-ca "pith-rogue-client-ca"

KEY_ALG=RSA KEY_OPT="-pkeyopt rsa_keygen_bits:2048" \
    make_leaf client-rsa "pith-client-rsa" client-ca -days "$days"
KEY_ALG=EC KEY_OPT="-pkeyopt ec_paramgen_curve:P-256" \
    make_leaf client-ecdsa "pith-client-ecdsa" client-ca -days "$days"
KEY_ALG=RSA KEY_OPT="-pkeyopt rsa_keygen_bits:2048" \
    make_leaf client-rogue "pith-rogue-client" rogue-ca -days "$days"
# already expired: a window that opened and closed before this was written, so
# the certificate is well formed and correctly signed and still must be refused.
KEY_ALG=RSA KEY_OPT="-pkeyopt rsa_keygen_bits:2048" \
    make_leaf client-expired "pith-expired-client" client-ca \
    -not_before 20200101000000Z -not_after 20210101000000Z

cp "$work/client-ca.crt" "$out/clientcert-ca.crt"
cp "$work/rogue-ca.crt" "$out/clientcert-rogue-ca.crt"
for pair in client-rsa:clientcert-rsa client-ecdsa:clientcert-ecdsa \
            client-rogue:clientcert-rogue client-expired:clientcert-expired; do
    src="${pair%%:*}"
    dst="${pair##*:}"
    cp "$work/$src.crt" "$out/$dst.crt"
    # std.net.tls reads pkcs#8 private keys; openssl genpkey already writes them.
    cp "$work/$src.key" "$out/$dst.key"
done

echo "wrote client certificate fixtures to $out"
