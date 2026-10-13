#!/bin/sh
# Gera dist/rumen-x86_64-linux.tar.gz + SHA256SUMS a partir da build release.
set -eu
cd "$(dirname "$0")/.."
test -x target/release/rumen || { echo "rode: cargo build --release" >&2; exit 1; }
rm -rf dist && mkdir -p dist/rumen
cp target/release/rumen dist/rumen/
cp packaging/rumen.desktop packaging/rumen.svg dist/rumen/
tar -C dist -czf dist/rumen-x86_64-linux.tar.gz rumen
(cd dist && sha256sum rumen-x86_64-linux.tar.gz > SHA256SUMS)
rm -rf dist/rumen
ls -lh dist
