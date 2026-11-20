#!/bin/bash
# Creates a self-signed code-signing identity in the login keychain.
#
# Ad-hoc signatures change on every build, and macOS ties a Screen Recording
# grant to that signature: every rebuild looks like a different app and asks
# again. Signing with a stable identity fixes that, because the grant is keyed
# to the certificate rather than to each binary's hash.
#
# codesign may ask for keychain access the first time; choose "Always Allow".
set -euo pipefail

NAME="${1:-Rewind Local Signing}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

if security find-identity -v -p codesigning 2>/dev/null | grep -q "$NAME"; then
    echo "$NAME"
    exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
    -keyout "$work/key.pem" -out "$work/cert.pem" \
    -subj "/CN=$NAME" \
    -addext "basicConstraints=critical,CA:false" \
    -addext "keyUsage=critical,digitalSignature" \
    -addext "extendedKeyUsage=critical,codeSigning" >/dev/null 2>&1

openssl pkcs12 -export -out "$work/identity.p12" \
    -inkey "$work/key.pem" -in "$work/cert.pem" \
    -passout pass:rewind >/dev/null 2>&1

security import "$work/identity.p12" -k "$KEYCHAIN" -P rewind \
    -T /usr/bin/codesign >/dev/null

echo "$NAME"
