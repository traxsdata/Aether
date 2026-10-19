#!/bin/sh
# Tests webnet's TLS 1.3 and 1.2 client against local `openssl s_server`s, with a
# throwaway CA made here. Every good case must load; every bad one must fail.
#   tools/test-webnet-tls.sh            (needs openssl >= 1.1.1 and cargo)
set -u
cd "$(dirname "$0")/.."
ROOT=$(pwd)
cargo build --release --quiet --manifest-path runner/Cargo.toml --bin webget || exit 1
WEBGET="$ROOT/runner/target/release/webget"
D=$(mktemp -d)
trap 'kill $(jobs -p) 2>/dev/null; rm -rf "$D"' EXIT
cd "$D"

o() { openssl "$@" 2>/dev/null; }

# ---- certificates ----------------------------------------------------------
cat > ca.cnf <<'EOF'
[ ca ]
default_ca = test
[ test ]
dir = .
database = index.txt
new_certs_dir = .
serial = serial
default_md = sha256
policy = any
copy_extensions = copy
unique_subject = no
[ any ]
commonName = supplied
[ req ]
distinguished_name = dn
prompt = no
[ dn ]
CN = placeholder
[ v3_ca ]
basicConstraints = critical, CA:true
keyUsage = critical, keyCertSign, cRLSign
subjectKeyIdentifier = hash
[ v3_notca ]
basicConstraints = critical, CA:false
keyUsage = critical, keyCertSign, digitalSignature
[ v3_leaf ]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = DNS:localhost, DNS:*.web.localhost, DNS:*.localhost
[ v3_leaf_other ]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = serverAuth
subjectAltName = DNS:other.example
[ v3_leaf_client ]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = clientAuth
subjectAltName = DNS:localhost
EOF
touch index.txt
echo 1000 > serial

key() { # name type
  case $2 in
    rsa2048) o genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out $1.key ;;
    rsa4096) o genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:4096 -out $1.key ;;
    p256) o genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out $1.key ;;
    p384) o genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-384 -out $1.key ;;
  esac
}
selfsigned() { # name cn type
  key $1 $3
  o req -new -x509 -key $1.key -subj "/CN=$2" -days 30 -config ca.cnf -extensions v3_ca -out $1.pem
}
issue() { # name cn type issuer ext [start end]
  key $1 $3
  o req -new -key $1.key -subj "/CN=$2" -config ca.cnf -out $1.csr
  if [ $# -ge 7 ]; then dates="-startdate $6 -enddate $7"; else dates="-days 30"; fi
  o ca -batch -config ca.cnf -cert $4.pem -keyfile $4.key -extensions $5 $dates -notext -in $1.csr -out $1.pem
}

selfsigned root "webnet test root" rsa2048
selfsigned eroot "webnet test EC root" p384
selfsigned other "some other root" rsa2048
issue inter "webnet test intermediate" p256 root v3_ca
issue notca "not a CA" rsa2048 root v3_notca
issue leaf_rsa localhost rsa2048 root v3_leaf
issue leaf_rsa4096 localhost rsa4096 root v3_leaf
issue leaf_p256 localhost p256 inter v3_leaf
issue leaf_p384 localhost p384 eroot v3_leaf
issue leaf_wrong other.example rsa2048 root v3_leaf_other
issue leaf_expired localhost rsa2048 root v3_leaf 20200101000000Z 20210101000000Z
issue leaf_future localhost rsa2048 root v3_leaf 20900101000000Z 20910101000000Z
issue leaf_client localhost rsa2048 root v3_leaf_client
issue leaf_untrusted localhost rsa2048 other v3_leaf
issue leaf_badchain localhost rsa2048 notca v3_leaf
cat root.pem eroot.pem > trust.pem

# ---- servers ---------------------------------------------------------------
PORT=24400
pass=0
fail=0
run() { # expect(ok|fail) description url-host -- s_server args...
  expect=$1; desc=$2; host=$3; shift 4
  PORT=$((PORT + 1))
  openssl s_server -quiet -accept $PORT -www "$@" >/dev/null 2>&1 &
  spid=$!
  sleep 0.4
  out=$("$WEBGET" --only-ca-file trust.pem --connect-to 127.0.0.1 "https://$host:$PORT/" 2>&1)
  rc=$?
  kill $spid 2>/dev/null
  wait $spid 2>/dev/null
  first=$(printf '%s\n' "$out" | grep -v '^webget: trusting' | head -1)
  if { [ $expect = ok ] && [ $rc = 0 ]; } || { [ $expect = fail ] && [ $rc != 0 ]; }; then
    pass=$((pass + 1)); echo "ok   $desc: $first"
  else
    fail=$((fail + 1)); echo "FAIL $desc (expected $expect): $first"
  fi
}

T13="-tls1_3"
for suite in TLS_AES_128_GCM_SHA256 TLS_AES_256_GCM_SHA384 TLS_CHACHA20_POLY1305_SHA256; do
  run ok "$suite, RSA-2048 leaf" localhost -- $T13 -ciphersuites $suite -cert leaf_rsa.pem -key leaf_rsa.key
  run ok "$suite, P-256 leaf via P-256 intermediate" localhost -- $T13 -ciphersuites $suite \
    -cert leaf_p256.pem -key leaf_p256.key -cert_chain inter.pem
done
run ok "RSA-4096 leaf" localhost -- $T13 -cert leaf_rsa4096.pem -key leaf_rsa4096.key
run ok "P-384 leaf under a P-384 root" localhost -- $T13 -cert leaf_p384.pem -key leaf_p384.key
run ok "RSA-PSS SHA-384 CertificateVerify" localhost -- $T13 -sigalgs rsa_pss_rsae_sha384 -cert leaf_rsa.pem -key leaf_rsa.key
run ok "RSA-PSS SHA-512 CertificateVerify" localhost -- $T13 -sigalgs rsa_pss_rsae_sha512 -cert leaf_rsa.pem -key leaf_rsa.key
run ok "wildcard *.web.localhost" www.web.localhost -- $T13 -cert leaf_rsa.pem -key leaf_rsa.key
run ok "server asks for a client certificate" localhost -- $T13 -verify 1 -cert leaf_rsa.pem -key leaf_rsa.key
run ok "server sends extra unrelated certs" localhost -- $T13 -cert leaf_p256.pem -key leaf_p256.key -cert_chain other.pem -cert_chain inter.pem

run fail "wrong host name" localhost -- $T13 -cert leaf_wrong.pem -key leaf_wrong.key
run fail "wildcard over a single-label name (*.localhost)" www.localhost -- $T13 -cert leaf_rsa.pem -key leaf_rsa.key
run fail "IP address not in the certificate" 127.0.0.1 -- $T13 -cert leaf_rsa.pem -key leaf_rsa.key
run fail "wildcard doesn't match two labels" a.b.web.localhost -- $T13 -cert leaf_rsa.pem -key leaf_rsa.key
run fail "expired certificate" localhost -- $T13 -cert leaf_expired.pem -key leaf_expired.key
run fail "certificate not valid yet" localhost -- $T13 -cert leaf_future.pem -key leaf_future.key
run fail "untrusted root" localhost -- $T13 -cert leaf_untrusted.pem -key leaf_untrusted.key -cert_chain other.pem
run fail "issued by a non-CA" localhost -- $T13 -cert leaf_badchain.pem -key leaf_badchain.key -cert_chain notca.pem
run fail "missing intermediate" localhost -- $T13 -cert leaf_p256.pem -key leaf_p256.key
run fail "client-auth-only certificate" localhost -- $T13 -cert leaf_client.pem -key leaf_client.key
run fail "RSA PKCS#1 CertificateVerify (forbidden in 1.3)" localhost -- $T13 -sigalgs rsa_pkcs1_sha256 -cert leaf_rsa.pem -key leaf_rsa.key
run ok "TLS 1.3 with P-256 key exchange only" localhost -- $T13 -groups P-256 -cert leaf_rsa.pem -key leaf_rsa.key
run ok "TLS 1.3 with P-384 key exchange only (HelloRetryRequest)" localhost -- $T13 -groups P-384 -cert leaf_rsa.pem -key leaf_rsa.key
run fail "TLS 1.3 with P-521 key exchange only" localhost -- $T13 -groups P-521 -cert leaf_rsa.pem -key leaf_rsa.key

# ---- TLS 1.2 ----------------------------------------------------------------
T12="-tls1_2"
for c in ECDHE-RSA-AES128-GCM-SHA256 ECDHE-RSA-AES256-GCM-SHA384 ECDHE-RSA-CHACHA20-POLY1305; do
  run ok "TLS 1.2 $c" localhost -- $T12 -cipher $c -cert leaf_rsa.pem -key leaf_rsa.key
done
for c in ECDHE-ECDSA-AES128-GCM-SHA256 ECDHE-ECDSA-AES256-GCM-SHA384 ECDHE-ECDSA-CHACHA20-POLY1305; do
  run ok "TLS 1.2 $c via intermediate" localhost -- $T12 -cipher $c -cert leaf_p256.pem -key leaf_p256.key -cert_chain inter.pem
done
run ok "TLS 1.2 P-384 leaf" localhost -- $T12 -cert leaf_p384.pem -key leaf_p384.key
run ok "TLS 1.2 P-256 key exchange" localhost -- $T12 -groups P-256 -cert leaf_rsa.pem -key leaf_rsa.key
run ok "TLS 1.2 X25519 key exchange" localhost -- $T12 -groups X25519 -cert leaf_rsa.pem -key leaf_rsa.key
run ok "TLS 1.2 RSA PKCS#1 SHA-256 signature" localhost -- $T12 -sigalgs rsa_pkcs1_sha256 -cert leaf_rsa.pem -key leaf_rsa.key
run ok "TLS 1.2 RSA-PSS SHA-384 signature" localhost -- $T12 -sigalgs rsa_pss_rsae_sha384 -cert leaf_rsa.pem -key leaf_rsa.key
run ok "TLS 1.2 ECDSA SHA-384 signature with a P-256 key" localhost -- $T12 -sigalgs ECDSA+SHA384 \
  -cert leaf_p256.pem -key leaf_p256.key -cert_chain inter.pem
run ok "TLS 1.2 server asks for a client certificate" localhost -- $T12 -verify 1 -cert leaf_rsa.pem -key leaf_rsa.key
run ok "TLS 1.2 RSA-4096 leaf" localhost -- $T12 -cert leaf_rsa4096.pem -key leaf_rsa4096.key

run fail "TLS 1.2 wrong host name" localhost -- $T12 -cert leaf_wrong.pem -key leaf_wrong.key
run fail "TLS 1.2 expired certificate" localhost -- $T12 -cert leaf_expired.pem -key leaf_expired.key
run fail "TLS 1.2 untrusted root" localhost -- $T12 -cert leaf_untrusted.pem -key leaf_untrusted.key -cert_chain other.pem
run fail "TLS 1.2 missing intermediate" localhost -- $T12 -cert leaf_p256.pem -key leaf_p256.key
run fail "TLS 1.2 CBC ciphers only" localhost -- $T12 -cipher ECDHE-RSA-AES128-SHA256:ECDHE-RSA-AES128-SHA -cert leaf_rsa.pem -key leaf_rsa.key
run fail "TLS 1.2 static RSA key exchange only" localhost -- $T12 -cipher AES128-GCM-SHA256 -cert leaf_rsa.pem -key leaf_rsa.key
run fail "TLS 1.2 finite-field DHE only" localhost -- $T12 -cipher DHE-RSA-AES128-GCM-SHA256 -cert leaf_rsa.pem -key leaf_rsa.key
run ok "TLS 1.2 P-384 key exchange only" localhost -- $T12 -groups P-384 -cert leaf_rsa.pem -key leaf_rsa.key
run fail "TLS 1.2 SHA-1 signatures only" localhost -- $T12 -sigalgs RSA+SHA1 -cipher 'DEFAULT:@SECLEVEL=0' -cert leaf_rsa.pem -key leaf_rsa.key
run fail "TLS 1.1 only server" localhost -- -tls1_1 -cipher 'DEFAULT:@SECLEVEL=0' -cert leaf_rsa.pem -key leaf_rsa.key

echo "$pass passed, $fail failed"
[ $fail = 0 ]
