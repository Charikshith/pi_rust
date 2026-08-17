#!/bin/bash
set -e

echo "=== Harness Initialization ==="

if [ ! -f Cargo.toml ]; then
  echo "No Cargo.toml yet — this repo is not initialized as a Rust project."
  echo "Bootstrap with: cargo init  (or: cargo init --lib)"
  echo "Once Cargo.toml exists, this script runs fmt / clippy / test / build."
  echo "=== Verification Skipped (uninitialized) ==="
  exit 0
fi

echo "=== cargo fmt --check ==="
cargo fmt --check

echo "=== cargo clippy -- -D warnings ==="
cargo clippy --all-targets -- -D warnings

echo "=== cargo test ==="
# Includes the byte-compat golden tests (crates/pirust-ai/tests/golden.rs), which
# round-trip real Pi fixtures against committed goldens — the oracle for byte-compat.
cargo test

echo "=== cargo build ==="
cargo build

# If node + the sibling Pi source are present, verify the committed goldens/corpus are
# not stale vs the source fixtures. Non-fatal when node or source is absent (the
# committed goldens still gate correctness via cargo test above).
if command -v node >/dev/null 2>&1; then
  echo "=== golden freshness (node --check) ==="
  node scripts/gen-golden.mjs --check || echo "WARN: goldens differ from source; run node scripts/gen-golden.mjs"
  node scripts/gen-message-corpus.mjs --check || echo "WARN: message corpus differs; run node scripts/gen-message-corpus.mjs"
  node scripts/gen-model-corpus.mjs --check || echo "WARN: model corpus differs; run node scripts/gen-model-corpus.mjs"
  node scripts/gen-rarefields-corpus.mjs --check || echo "WARN: rare-fields corpus differs; run node scripts/gen-rarefields-corpus.mjs"
  # feat-004 tool fixtures: schemas/strings, truncate, edit diff corpus, exec corpus.
  node scripts/gen-tools-oracle.mjs --check || echo "WARN: tool fixtures differ; run node scripts/gen-tools-oracle.mjs"
  # feat-005 CLI/config fixtures: argv corpus, help text, settings merge, migrations.
  if [ -f scripts/gen-cli-oracle.mjs ]; then
    node scripts/gen-cli-oracle.mjs --check || echo "WARN: cli fixtures differ; run node scripts/gen-cli-oracle.mjs"
  fi
  # feat-005 Wave 4 sdk fixtures: system-prompt + provider-attribution.
  if [ -f scripts/gen-sdk-oracle.mjs ]; then
    node scripts/gen-sdk-oracle.mjs --check || echo "WARN: sdk fixtures differ; run node scripts/gen-sdk-oracle.mjs"
  fi
  # events.corpus.jsonl is a frozen capture (non-deterministic ids) — not --check'd.
fi
# agent-core oracle fixtures (tests/fixtures/pi/agent/*: entries/header/uuid/loop/compaction)
# are committed and gated by cargo test's byte/vector golden suites above. Regenerate
# manually (needs the sibling ../pi repo) via: node scripts/gen-agent-oracle.mjs

echo "=== Verification Complete ==="
echo ""
echo "Next steps:"
echo "1. Read feature_list.json to see current feature state"
echo "2. Pick ONE unfinished feature to work on"
echo "3. Implement only that feature"
echo "4. Re-run ./init.sh before claiming done"
