#!/usr/bin/env bash
# Full system check as run by CI: verify every source, build the whole system
# from source and boot it. Expects a fresh Ubuntu 26.04 with root privileges
# (e.g. a privileged container) and ideally /dev/kvm.
set -euo pipefail
cd "$(dirname "$0")/.."

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
grep -vE '^\s*(#|$)' ci/packages.txt \
    | xargs apt-get install -y -qq --no-install-recommends >/dev/null

if ! command -v cargo >/dev/null; then
    curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none -q
    source "$HOME/.cargo/env"
fi

./bz verify
./bz build
./bz smoke
