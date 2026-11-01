#!/bin/bash
# A measurement build for the owner's PC, in three steps (CLAUDE.md, "Messbau"):
#
#   ci/measure.sh start 0.5.128   branch measure/desktop-0.5.128 from main, with
#                                 the last one's hw-test.sh and kernel/measure;
#                                 then edit hw-test.sh (only what this build
#                                 measures) and add MEASURE lines to the code
#   ci/measure.sh finish          commit, tag, build, the whole smoke test, and
#                                 the stick's files in build/out/desktop/stick;
#                                 the owner then runs `./bz release`
#   ci/measure.sh stick           the release and the stick's files onto D:,
#                                 compared byte by byte; then back to main
#
# Measurement code never reaches main: it lives on its branch only.
set -euo pipefail
cd "$(dirname "$0")/.."

stick_dir=build/out/desktop/stick
fail() { echo "measure: $*" >&2; exit 1; }

case ${1:-} in
start)
    version=${2:-}
    [[ $version =~ ^0\.5\.[0-9]+$ ]] || fail "start needs a version like 0.5.128"
    [ "$(git branch --show-current)" = main ] || fail "start from main"
    [ -z "$(git status --porcelain)" ] || fail "main has uncommitted changes"
    git rev-parse -q --verify "refs/tags/measure/desktop-$version" >/dev/null \
        && fail "measure/desktop-$version exists already"
    last=$(git tag -l 'measure/desktop-*' --sort=-v:refname | head -n 1)
    git checkout -q -b "measure/desktop-$version"
    mkdir -p measure
    git show "$last:measure/hw-test.sh" | sed "s/0\.5\.[0-9]\+/$version/g" > measure/hw-test.sh
    chmod +x measure/hw-test.sh
    if git cat-file -e "$last:kernel/measure" 2>/dev/null; then
        git checkout "$last" -- kernel/measure
    fi
    sed -i "s/^pub const OS_VERSION: &str = \".*\";/pub const OS_VERSION: \&str = \"$version\";/" \
        tools/bz/src/layout.rs
    echo "measure/desktop-$version from $last: edit measure/hw-test.sh (only what this"
    echo "build measures; looks go into the release message), add MEASURE lines, then"
    echo "ci/measure.sh finish"
    ;;
finish)
    branch=$(git branch --show-current)
    [[ $branch =~ ^measure/desktop-(0\.5\.[0-9]+)$ ]] || fail "finish on a measurement branch"
    version=${BASH_REMATCH[1]}
    bash -n measure/hw-test.sh || fail "measure/hw-test.sh does not parse"
    grep -q "\"$version\"" tools/bz/src/layout.rs || fail "layout.rs is not at $version"
    git add -A
    git commit -q -m "measure/desktop: version $version" -m "Measurement build on main (ci/measure.sh): main, kernel/measure, hw-test.sh and the MEASURE lines of this build.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
    git tag -f "measure/desktop-$version" >/dev/null
    ./bz build </dev/null
    ./bz smoke </dev/null
    rm -rf "$stick_dir"
    mkdir -p "$stick_dir"
    cp build/sysroot/usr/lib/beize/test/{beize-testclient,beize-ui-test,beize-load-test,beize-terminal-sample} \
        measure/hw-test.sh "$stick_dir/"
    (cd "$stick_dir" && sha256sum -- * > SHA256SUMS)
    echo "measure/desktop-$version built and smoked; stick files in $stick_dir."
    echo "The owner: ./bz release (1Password), then 'Release fertig'."
    ;;
stick)
    branch=$(git branch --show-current)
    [[ $branch =~ ^measure/desktop-(0\.5\.[0-9]+)$ ]] || fail "stick on the measurement branch"
    version=${BASH_REMATCH[1]}
    release=build/out/desktop/release/$version
    [ -d "$release" ] || fail "no release $release: the owner runs ./bz release first"
    [ -d "$stick_dir" ] || fail "no stick files: ci/measure.sh finish first"
    mountpoint -q /mnt/d || sudo -n mount -t drvfs D: /mnt/d || fail "the stick is not at D:"
    mkdir -p "/mnt/d/beize-$version" /mnt/d/beize-test
    cp -r "$release"/. "/mnt/d/beize-$version/"
    cp "$stick_dir"/* /mnt/d/beize-test/
    for f in "$release"/*; do cmp -s "$f" "/mnt/d/beize-$version/$(basename "$f")" || fail "differs: $f"; done
    for f in "$stick_dir"/*; do cmp -s "$f" "/mnt/d/beize-test/$(basename "$f")" || fail "differs: $f"; done
    sync
    sudo -n umount /mnt/d
    git checkout -q main
    echo "On the stick, compared: D:\\beize-$version and D:\\beize-test. Back on main."
    ;;
*)
    sed -n '2,15p' "$0"
    exit 1
    ;;
esac
