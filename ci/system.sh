#!/usr/bin/env bash
# Full system check as run by CI: build the whole system from source and boot
# it. Runs in Beize's buildroot (ADR 0021) – the CI's container image – with
# root privileges and ideally /dev/kvm.
#
# Two levels, as in Chromium's and Fedora's CI: SMOKE=quick on every push
# (install and two boots of each variant with all checks of an ordinary
# start: a fixed budget of about five minutes), SMOKE=full every night and
# before every release (every scenario, on the desktop – what computers
# get). New scenarios go into the full level, never into every push.
set -euo pipefail
cd "$(dirname "$0")/.."

SMOKE=${SMOKE:-full}
case $SMOKE in
    quick) quick=--quick ;;
    full) quick= ;;
    *) echo "SMOKE must be quick or full, not $SMOKE" >&2; exit 1 ;;
esac

# What the build runs on (build jobs and test VMs follow from it).
echo "build host: $(nproc) CPUs, $(awk '/MemTotal/ {printf "%.1f", $2 / 1048576}' /proc/meminfo) GiB, $(df -h --output=avail . | tail -1) free"
echo "buildroot: $(cat /etc/beize-buildroot)"

# Every source's signature again (CI: weekly and by hand); a build verifies
# new sources itself.
if [ "${VERIFY_SOURCES:-true}" = true ]; then
    ./bz verify
fi
# The base on its own (ADR 0025): it builds and passes its smoke test without
# a package of the desktop. Free space after each step: a full build is close
# to what a runner has.
./bz build --locked --variant base
ci/digest.sh
df -h .
./bz smoke --quick --variant base
# The desktop on top, from the same packages.
./bz build --locked --variant desktop
df -h .
./bz smoke $quick --variant desktop
df -h .
