#!/usr/bin/env bash
# Prints the name of Beize's buildroot (ADR 0021): a hash of everything that
# decides what is in it. Changing any of these files makes a new buildroot,
# and bz rebuilds every package in it.
set -euo pipefail
cd "$(dirname "$0")/../.."
cat ci/buildroot/create.sh ci/buildroot/packages.txt ci/buildroot/packages.lock \
    ci/buildroot/snapshot rust-toolchain.toml | sha256sum | cut -c1-16
