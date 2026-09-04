#!/usr/bin/env bash
# Generates the self-signed code-signing certificate (p12) the release
# workflow signs the macOS bundle with. Works anywhere openssl exists
# (Git Bash included). Run once:
#   bash packaging/macos/make-self-signed-cert.sh
# Then add the GitHub secrets: MACOS_P12_BASE64 (printed below) and
# MACOS_P12_PASSWORD (the password you typed). Keep the .p12 safe and
# never commit it.
set -euo pipefail

read -rsp "P12 password: " PASS
echo

DIR=$(mktemp -d)
trap 'rm -rf "$DIR"' EXIT

openssl req -x509 -newkey rsa:4096 -sha256 -days 3650 -nodes \
    -keyout "$DIR/key.pem" -out "$DIR/cert.pem" \
    -subj "/CN=Zapive Self-Signed" \
    -addext "keyUsage=critical,digitalSignature" \
    -addext "extendedKeyUsage=critical,codeSigning" \
    -addext "basicConstraints=critical,CA:FALSE"

openssl pkcs12 -export -inkey "$DIR/key.pem" -in "$DIR/cert.pem" \
    -name "Zapive Self-Signed" -passout "pass:$PASS" -out zapive-selfsigned.p12

echo "P12 written to zapive-selfsigned.p12"
echo "MACOS_P12_BASE64:"
openssl base64 -A -in zapive-selfsigned.p12
echo
