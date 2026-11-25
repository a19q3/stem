#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
output="$(realpath -m "${1:?usage: sdk/wasm/build.sh OUTPUT_DIRECTORY}")"
bindgen="${STEM_WASM_BINDGEN:-wasm-bindgen}"
cd "$root"
if [[ "$("$bindgen" --version)" != "wasm-bindgen 0.2.118" ]]; then
    printf '%s\n' 'Use wasm-bindgen-cli 0.2.118 (cargo install --locked --version 0.2.118 wasm-bindgen-cli).' >&2
    exit 1
fi
target="$(cargo metadata --locked --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
cargo build --locked --release --target wasm32-unknown-unknown -p stem-wasm
mkdir -p "$output"
"$bindgen" --target web --out-dir "$output" "$target/wasm32-unknown-unknown/release/stem_wasm.wasm"
cp "$root/sdk/wasm/package.json" "$root/sdk/wasm/README.md" "$root/LICENSE" "$output/"
