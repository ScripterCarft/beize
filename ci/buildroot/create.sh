#!/usr/bin/env bash
# Creates Beize's buildroot (ADR 0021) as a tar archive: Ubuntu 26.04 with
# exactly the packages of packages.lock (lock.sh), the Rust toolchain of
# rust-toolchain.toml and the cargo tools below. bz runs in it, locally
# (./bz, with bwrap) as in the CI (as the jobs' container image).
#
#   ci/buildroot/create.sh <archive.tar> [<cache directory for packages>]
#
# Unprivileged with subordinate IDs (mmdebstrap's unshare mode), or as root.
# Needs mmdebstrap and curl.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)

# cargo-deny for the CI's checks of the Rust dependencies: the project's
# release build, pinned by the checksum it publishes.
CARGO_DENY=https://github.com/EmbarkStudios/cargo-deny/releases/download/0.20.2/cargo-deny-0.20.2-x86_64-unknown-linux-musl.tar.gz
CARGO_DENY_SHA256=9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f
# cargo-audit for `bz cve`: RustSec advisories of every crate in the image,
# also those of other projects' Cargo.lock (ntpd-rs), which cargo-deny cannot
# read without their manifest. Pinned by the digest GitHub publishes.
CARGO_AUDIT=https://github.com/rustsec/rustsec/releases/download/cargo-audit%2Fv0.22.2/cargo-audit-x86_64-unknown-linux-musl-v0.22.2.tgz
CARGO_AUDIT_SHA256=7fb9497f8594b389e5fce5ef9b92db08432996895b2e0c5a0167a69ed445c428

ARCHIVE=http://archive.ubuntu.com/ubuntu
SNAPSHOT=https://snapshot.ubuntu.com/ubuntu/$(< "$here/snapshot")

# One package of the lock into the cache: from Ubuntu's archive, whose pool
# is fast and holds every current version; from the snapshot when the
# archive dropped it. Either way it must have the locked checksum.
if [ "${1:-}" = --fetch ]; then
    cache=$2 file=$3 sha256=$4
    dest=$cache/${file##*/}
    if [ -e "$dest" ] && echo "$sha256  $dest" | sha256sum --check --status; then
        exit 0
    fi
    for base in "$ARCHIVE" "$SNAPSHOT"; do
        if curl -fsS --retry 3 -o "$dest.part" "$base/$file" 2>/dev/null &&
            echo "$sha256  $dest.part" | sha256sum --check --status; then
            mv "$dest.part" "$dest"
            exit 0
        fi
    done
    rm -f "$dest.part"
    echo "error: ${file##*/}: not found with its locked checksum" >&2
    exit 1
fi

# Second stage, run by mmdebstrap inside the new tree.
if [ "${1:-}" = --customize ]; then
    root=$2
    # Exactly the lock, nothing more or less.
    if ! diff <(grep -v '^#' "$here/packages.lock" | cut -d' ' -f1-3) \
        <(chroot "$root" dpkg-query -W -f '${Package} ${Version} ${Architecture}\n' | sort); then
        echo "error: the buildroot differs from packages.lock (above)" >&2
        exit 1
    fi
    rust=$(< "$here/rust")
    in_root() {
        chroot "$root" env RUSTUP_HOME=/opt/rust CARGO_HOME=/opt/rust \
            PATH=/opt/rust/bin:/usr/bin:/bin "$@"
    }
    in_root rustup toolchain install "$rust" --profile minimal \
        --component rustfmt,clippy
    in_root rustup default "$rust"
    mkdir -p "$root/opt/rust/bin"
    tools=$(mktemp -d)
    curl -fsSL -o "$tools/cargo-deny.tar.gz" "$CARGO_DENY"
    echo "$CARGO_DENY_SHA256  $tools/cargo-deny.tar.gz" | sha256sum --check --status
    tar -x -z -f "$tools/cargo-deny.tar.gz" -C "$tools" --strip-components=1
    install -m 0755 "$tools/cargo-deny" "$root/opt/rust/bin/"
    curl -fsSL -o "$tools/cargo-audit.tgz" "$CARGO_AUDIT"
    echo "$CARGO_AUDIT_SHA256  $tools/cargo-audit.tgz" | sha256sum --check --status
    tar -x -z -f "$tools/cargo-audit.tgz" -C "$tools" --strip-components=1
    install -m 0755 "$tools/cargo-audit" "$root/opt/rust/bin/"
    rm -rf "$tools"
    # What the buildroot is (bz compares it with ci/buildroot/hash.sh) and
    # what it holds, as Debian's .buildinfo records a build's environment.
    cp "$here/name" "$root/etc/beize-buildroot"
    mkdir -p "$root/usr/share/beize-buildroot"
    {
        chroot "$root" dpkg-query -W -f '${Package} ${Version} ${Architecture}\n'
        in_root rustc --version
        in_root cargo deny --version
        in_root cargo audit --version
    } > "$root/usr/share/beize-buildroot/buildinfo"
    exit 0
fi

out=${1:?usage: create.sh <archive.tar> [<package cache>]}
cache=${2:-$(mktemp -d)}
mkdir -p "$cache"
grep -v '^#' "$here/packages.lock" | cut -d' ' -f4,5 |
    xargs -P 16 -n 2 "$here/create.sh" --fetch "$cache"
packages=$(grep -vE '^\s*(#|$)' "$here/packages.txt" | paste -sd, -)
# What the second stage needs, where it can read it: in unshare mode it
# runs as root of a user namespace, to which the user's home is closed.
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp "$here/create.sh" "$here/packages.lock" "$here/snapshot" "$stage/"
"$here/hash.sh" > "$stage/name"
sed -n 's/^channel = "\(.*\)"$/\1/p' "$here/../../rust-toolchain.toml" > "$stage/rust"
chmod -R a+rX "$stage"
# apt resolves against the snapshot's indexes, as lock.sh did, and finds
# every package already in its cache (checked again against the indexes).
# The snapshot's Release files may be older than Valid-Until allows.
# snapshot.ubuntu.com answers 5xx at times (2026-10-14: twice in an hour):
# apt tries each download again, with growing pauses.
mmdebstrap --variant=apt --components=main,universe \
    --include="$packages" \
    --aptopt='Acquire::Retries "8"' \
    --dpkgopt='path-exclude=/usr/share/doc/*' \
    --dpkgopt='path-include=/usr/share/doc/*/copyright' \
    --dpkgopt='path-exclude=/usr/share/man/*' \
    --dpkgopt='path-exclude=/usr/share/info/*' \
    --aptopt='Acquire::Check-Valid-Until "false"' \
    --setup-hook='mkdir -p "$1/var/cache/apt/archives"' \
    --setup-hook="copy-in $(printf '%q ' "$cache"/*.deb) /var/cache/apt/archives/" \
    --skip=download/empty \
    --customize-hook="\"$stage/create.sh\" --customize \"\$1\"" \
    resolute "$out" \
    "deb $SNAPSHOT resolute main universe" \
    "deb $SNAPSHOT resolute-updates main universe" \
    "deb $SNAPSHOT resolute-security main universe"
