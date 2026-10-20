#!/usr/bin/env bash
# Smoke test of the release binary, end to end over a real port:
# serve → send (big file + nested dir) → ls → get --all → cmp → wrong token.
# Prints the measured throughput used in the README.
#
#   scripts/smoke.sh [binary]      SIZE_MIB=1024 PORT=7979 scripts/smoke.sh
set -euo pipefail

BIN=${1:-target/release/fireshare}
SIZE_MIB=${SIZE_MIB:-256}
PORT=${PORT:-7979}
TOKEN=smoke-$RANDOM$RANDOM
URL=http://127.0.0.1:$PORT

[ -x "$BIN" ] || { echo "❌ binary $BIN not found (make release)"; exit 1; }
WORK=$(mktemp -d)
cleanup() { [ -n "${PID:-}" ] && kill "$PID" 2>/dev/null; wait 2>/dev/null; rm -rf "$WORK"; }
trap cleanup EXIT

"$BIN" serve --dir "$WORK/shared" --bind 127.0.0.1 --port "$PORT" --token "$TOKEN" >"$WORK/serve.log" 2>&1 &
PID=$!
for _ in $(seq 1 100); do curl -sf "$URL/health" >/dev/null && break; sleep 0.05; done
curl -sf "$URL/health" >/dev/null || { cat "$WORK/serve.log"; echo "❌ server did not start"; exit 1; }

mkdir -p "$WORK/src/proj/src" "$WORK/out"
head -c $((SIZE_MIB * 1024 * 1024)) /dev/urandom >"$WORK/src/big.bin"
echo "# smoke" >"$WORK/src/proj/README.md"
echo 'fn main() {}' >"$WORK/src/proj/src/main.rs"

export FIRESHARE_SERVER=$URL FIRESHARE_TOKEN=$TOKEN
echo "📤 send ($SIZE_MIB MiB + directory)"
"$BIN" send "$WORK/src/big.bin" "$WORK/src/proj" --as smoke | tail -1
echo "📋 ls"
"$BIN" ls
echo "📥 get --all"
"$BIN" get --all -o "$WORK/out" | tail -1

cmp "$WORK/src/big.bin" "$WORK/out/smoke/big.bin"
cmp "$WORK/src/proj/src/main.rs" "$WORK/out/smoke/proj/src/main.rs"
echo "🔎 cmp: identical"

if FIRESHARE_TOKEN=wrong "$BIN" ls >/dev/null 2>&1; then
  echo "❌ wrong token was accepted"; exit 1
fi
echo "🔒 wrong token: refused"
echo "✅ smoke ok"
