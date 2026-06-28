#!/usr/bin/env bash
# Retry cargo when crates.io downloads flake (HTTP/2 framing, transient curl errors).
set -euo pipefail

export CARGO_NET_RETRY="${CARGO_NET_RETRY:-10}"
export CARGO_HTTP_MULTIPLEXING="${CARGO_HTTP_MULTIPLEXING:-false}"

attempts="${CARGO_RETRY_ATTEMPTS:-3}"
delay="${CARGO_RETRY_DELAY_SECS:-15}"

for ((i = 1; i <= attempts; i++)); do
  if cargo "$@"; then
    exit 0
  fi
  if ((i == attempts)); then
    exit 1
  fi
  echo "cargo $* failed (attempt ${i}/${attempts}), retrying in ${delay}s..." >&2
  sleep "$delay"
done
