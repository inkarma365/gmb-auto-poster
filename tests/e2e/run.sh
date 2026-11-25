#!/usr/bin/env bash
# Test end-to-end su una pagina finta che imita il pannello della scheda:
# pulsante → dialog in iframe → textarea → selettore file → Pubblica.
# Uso: tests/e2e/run.sh <binario gmb-poster> <eseguibile chromium/chrome>
set -euo pipefail
BIN=$(realpath "$1"); CHROME=$2
HERE=$(cd "$(dirname "$0")" && pwd); ROOT=$(cd "$HERE/../.." && pwd)
W=$(mktemp -d); trap 'rm -rf "$W"' EXIT
cp "$HERE/fake.html" "$W/"
mkdir -p "$W/posts/queue/001-e2e"
printf 'Post di prova e2e 🎉\nSeconda riga.' > "$W/posts/queue/001-e2e/post.txt"
python3 -I - "$W/posts/queue/001-e2e/foto.png" <<'PY'
import sys, zlib, struct, os
w = h = 400
raw = b''.join(b'\x00' + os.urandom(w * 3) for _ in range(h))
def chunk(t, d): return struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
open(sys.argv[1], 'wb').write(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b''))
PY
sed -e "s|^business_url.*|business_url = \"file://$W/fake.html\"|" \
    -e 's|^warmup_url.*|warmup_url = ""|' \
    -e "s|^chromium_path.*|chromium_path = \"$CHROME\"|" \
    -e 's|^headless.*|headless = true\nno_sandbox = true|' \
    -e 's|^step_delay_ms.*|step_delay_ms = [200, 400]|' \
    -e 's|^page_settle_ms.*|page_settle_ms = [500, 800]|' \
    -e 's|^type_delay_ms.*|type_delay_ms = [2, 8]|' \
    -e 's|^element_timeout_s.*|element_timeout_s = 10|' \
    "$ROOT/config.toml.example" > "$W/config.toml"
sed -e 's|ms = \[[0-9]*, [0-9]*\]|ms = [300, 500]|' -e 's|^timeout_s = 6$|timeout_s = 2|' "$ROOT/flow.toml" > "$W/flow.toml"

cd "$W"
echo "== dry-run"
"$BIN" post --dry-run
test -d posts/queue/001-e2e || { echo "FAIL: il dry-run ha spostato il post"; exit 1; }

echo "== post"
"$BIN" post
test -d posts/published/*-001-e2e || { echo "FAIL: post non archiviato"; exit 1; }
grep -q '"001-e2e"' state.json || { echo "FAIL: state.json non aggiornato"; exit 1; }
html=$(ls screenshots/*-pubblicato.html)
for want in 'file=foto.png' 'trusted=true' 'webdriver=false' 'Post di prova e2e 🎉\nSeconda riga.'; do
  grep -qF "$want" "$html" || { echo "FAIL: manca '$want' nella pagina"; grep -o 'PUBBLICATO[^<]*' "$html" || true; exit 1; }
done
grep -o 'PUBBLICATO[^<]*' "$html"

echo "== rate limit"
out=$("$BIN" post 2>&1)
echo "$out"
grep -q 'già pubblicati 1 post oggi' <<<"$out" || { echo "FAIL: il limite giornaliero non ha bloccato"; exit 1; }
echo "E2E OK"
