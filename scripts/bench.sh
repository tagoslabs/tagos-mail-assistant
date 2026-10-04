#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -p tagos-mail-assistant-demo --offline
exec ./target/debug/tagos-mail-assistant-demo bench
