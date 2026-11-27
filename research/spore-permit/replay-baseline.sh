#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "Usage: $0 STEM_BINARY WASM_SDK_DIRECTORY" >&2
    exit 2
fi

base="$(cd "$(dirname "$0")" && pwd)"
study="$base/upstream/collections"
wallet="$1"
sdk="$2"

python3 "$base/check-deposit.py" --stem "$wallet"
python3 "$study/check.py" --stem "$wallet"
node "$study/../beta2/native.mjs" "$sdk" "$study"
python3 "$study/dependencies.py" --stem "$wallet"
node "$study/../beta2/native.mjs" "$sdk" "$study/issuance-deps"
