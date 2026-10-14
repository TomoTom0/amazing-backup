#!/usr/bin/env bash
# one-click operational verification for backup-remote.
#
# Uses a small generated archive and isolated data/out + registry.
# By default this only runs dry-run upload.
# Set --live (or LIVE=1) to perform PutObject against the configured bucket.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="${TMPDIR:-/tmp}/backup-remote-operational-$$"
mkdir -p "$TMP/test"
ZIP="$TMP/test/verify.zip"
python3 -c 'import zipfile; z=zipfile.ZipFile("'"$ZIP"'","w"); z.writestr("hello.txt","hello backup-remote operational verification"); z.close()'

PIPELINE="$TMP/pipeline.toml"
cat > "$PIPELINE" <<PIPELINE_TOML
[recipients]
public_keys = ["$ROOT/configs/secret/keys/private/backup_ed25519.pub"]

[output]
dir = "$TMP/out"
registry = "$TMP/registry.jsonl"

[behavior]
keep_going = false

[verify]
decrypt_key = "$ROOT/configs/secret/keys/private/backup_ed25519"
PIPELINE_TOML

cd "$ROOT"
echo "== plan =="
cargo run --quiet -- plan --config "$PIPELINE" "$ZIP"

echo "== encrypt =="
cargo run --quiet -- encrypt --config "$PIPELINE" --out "$TMP/out" --registry "$TMP/registry.jsonl" "$ZIP"

echo "== upload dry-run =="
cargo run --quiet -- upload --config "$PIPELINE" --aws-config "$ROOT/configs/aws.toml" --dry-run

if [[ "${1:-}" == "--live" || "${LIVE:-}" == "1" ]]; then
    echo "== upload (live) =="
    cargo run --quiet -- upload --config "$PIPELINE" --aws-config "$ROOT/configs/aws.toml"
    echo "== verify =="
    cargo run --quiet -- verify --config "$PIPELINE"
else
    echo "dry-run only (pass --live or LIVE=1 to perform PutObject)"
fi

echo "artifacts: $TMP"
