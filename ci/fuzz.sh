#!/usr/bin/env bash
# Fuzzes what unprivileged callers send Beize's services: each target in
# fuzz/ for the given seconds (default 60) with cargo-fuzz (libFuzzer and
# AddressSanitizer), as OSS-Fuzz and systemd do. In the buildroot: locally
# `./bz exec ci/fuzz.sh [seconds]`, in the CI weekly.
#
# Fuzzing needs a nightly compiler; it and cargo-fuzz are pinned here and
# kept in build/fuzz, not in the buildroot, whose every other job would
# carry them. Findings go to build/fuzz/artifacts/<target>/; a fixed one
# goes into fuzz/regressions/<target>/, which `cargo test` replays.
set -euo pipefail
cd "$(dirname "$0")/.."

NIGHTLY=nightly-2026-10-01
CARGO_FUZZ=https://github.com/rust-fuzz/cargo-fuzz/releases/download/0.13.2/cargo-fuzz-0.13.2-x86_64-unknown-linux-musl.tar.gz
# Taken on 2026-10-13; the project publishes no checksums.
CARGO_FUZZ_SHA256=b5b704018b63e0f151c17a057ac53b5111e1db545d1b9f72fee79f08a545931c
seconds=${1:-60}
# cargo-fuzz is built for musl and would take that as the target.
TARGET=x86_64-unknown-linux-gnu

work=$PWD/build/fuzz
mkdir -p "$work/bin"
export RUSTUP_HOME=$work/rustup
export CARGO_HOME=${CARGO_HOME:-$PWD/build/cache/cargo}
export PATH=$work/bin:$PATH
rustup toolchain install "$NIGHTLY" --profile minimal --no-self-update >/dev/null
if [ ! -x "$work/bin/cargo-fuzz" ]; then
    curl -fsSL -o "$work/cargo-fuzz.tar.gz" "$CARGO_FUZZ"
    echo "$CARGO_FUZZ_SHA256  $work/cargo-fuzz.tar.gz" | sha256sum --check --status
    tar -x -z -f "$work/cargo-fuzz.tar.gz" -C "$work/bin"
    rm "$work/cargo-fuzz.tar.gz"
fi

for target in $(cargo "+$NIGHTLY" fuzz list --fuzz-dir fuzz); do
    echo "==> Fuzzing $target for ${seconds}s"
    # A corpus of its own in build/: what it finds grows there, the seeds in
    # fuzz/corpus stay as committed.
    mkdir -p "$work/corpus/$target" "$work/artifacts/$target"
    cargo "+$NIGHTLY" fuzz run --fuzz-dir fuzz --target "$TARGET" "$target" \
        "$work/corpus/$target" "fuzz/corpus/$target" -- \
        -max_total_time="$seconds" -artifact_prefix="$work/artifacts/$target/" \
        -print_final_stats=1
done
