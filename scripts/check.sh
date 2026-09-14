#!/usr/bin/env bash
# Тихая проверка: форматирование, clippy без предупреждений, тесты. Печатает только ошибки.
set -euo pipefail
cd "$(dirname "$0")/.."

step() {
  local name=$1
  shift
  if ! out=$("$@" 2>&1); then
    echo "✗ $name"
    echo "$out" | tail -60
    exit 1
  fi
}

step "cargo fmt" cargo fmt --all --check
step "cargo clippy" cargo clippy -q --all-targets --workspace -- -D warnings
step "cargo test" cargo test -q --workspace
echo "✓ fmt, clippy, tests"
