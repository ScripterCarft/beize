#!/usr/bin/env bash
# What every build machine must produce alike from the same sources (#5):
# the parts of a build that no key has touched yet – the kernel's modules,
# the initrd with and without its modules, the /usr tree. One SHA-256 each;
# run it in the CI and locally and compare. The kernel image and the signed
# parts differ by design: they carry each key set's certificate and
# signatures (ADR 0015).
set -euo pipefail
cd "$(dirname "$0")/../build"

digest() {
    find "$@" -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum | cut -c1-16
}

echo "unsigned parts of this build (ci/digest.sh):"
echo "  modules        $(digest out/modules)"
echo "  initrd         $(digest out/initrd.cpio.zst)"
echo "  initrd modules $(digest work/initrd-modules)"
echo "  /usr tree      $(digest work/usr-tree)"
