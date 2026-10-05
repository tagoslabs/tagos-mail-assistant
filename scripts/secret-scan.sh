#!/usr/bin/env bash
# Local release check. Patterns live outside the repo so this script
# cannot leak them. Stanislav fills the denylist before the first push.
set -euo pipefail
cd "$(dirname "$0")/.."

deny="${TAGOS_PRIVATE_DENYLIST:-$HOME/.config/tagos-mail-assistant/denylist.txt}"
fail=0

echo "authors"
mapfile -t authors < <(git log --format='%ae' | sort -u)
expected="48457923+tagoslabs@users.noreply.github.com"
if [ "${#authors[@]}" -eq 1 ] && [ "${authors[0]}" = "$expected" ]; then
  echo "authors: $expected"
else
  echo "unexpected commit authors:" "${authors[@]}"
  fail=1
fi

if [ -s "$deny" ]; then
  echo "denylist: $deny"
  set +e
  rg -n -f "$deny" .
  tree_rc=$?
  set -e
  if [ "$tree_rc" -eq 0 ]; then
    echo "denylist hit in the tree"
    fail=1
  elif [ "$tree_rc" -ne 1 ]; then
    echo "rg failed on the tree ($tree_rc)"
    exit "$tree_rc"
  else
    echo "denylist: no hits in the tree"
  fi

  set +e
  git log -p --all | rg -n -f "$deny" -
  hist_rc=${PIPESTATUS[1]}
  set -e
  if [ "$hist_rc" -eq 0 ]; then
    echo "denylist hit in history"
    fail=1
  elif [ "$hist_rc" -ne 1 ]; then
    echo "rg failed on history ($hist_rc)"
    exit "$hist_rc"
  else
    echo "denylist: no hits in history"
  fi
else
  echo "private denylist missing or empty ($deny); pattern scan skipped"
fi

echo "gitleaks"
docker run --rm -v "$PWD:/repo" \
  ghcr.io/gitleaks/gitleaks:v8.30.1 \
  detect --source=/repo --config=/repo/.gitleaks.toml --redact --verbose

echo "cargo deny"
cargo deny check licenses advisories

if [ "$fail" -ne 0 ]; then
  exit "$fail"
fi
echo "secret scan ok"
