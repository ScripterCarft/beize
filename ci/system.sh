#!/usr/bin/env bash
# Full system check as run by CI: verify every source, build the whole system
# from source and boot it. Runs in Beize's buildroot (ADR 0021) – the CI's
# container image – with root privileges and ideally /dev/kvm.
set -euo pipefail
cd "$(dirname "$0")/.."

# What the build runs on (build jobs and test VMs follow from it).
echo "build host: $(nproc) CPUs, $(awk '/MemTotal/ {printf "%.1f", $2 / 1048576}' /proc/meminfo) GiB, $(df -h --output=avail . | tail -1) free"
echo "buildroot: $(cat /etc/beize-buildroot)"

# Every source's signature again (CI: weekly and by hand); a build verifies
# new sources itself.
if [ "${VERIFY_SOURCES:-true}" = true ]; then
    ./bz verify
fi
# Free space after each step: a full build is close to what a runner has.
./bz build --locked
ci/digest.sh
df -h .
./bz smoke
df -h .
