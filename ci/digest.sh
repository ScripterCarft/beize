#!/usr/bin/env bash
# What every build machine must produce alike from the same sources (#5):
# the parts of a build that no key has touched yet – the kernel's modules,
# the initrd with and without its modules, the base's /usr tree and image. One
# SHA-256 each; run it in the CI and locally and compare. The kernel image
# and the signed parts differ by design: they carry each key set's
# certificate and signatures (ADR 0015).
#
# A tree's digest covers each entry's type, mode, symlink target and
# content, not only file contents: a mode that depends on the builder (the
# umask, #5) must show. Owners are left out – the trees belong to whoever
# builds; the images record root. The lists behind the digests go to
# out/digest/ (the CI keeps them), so a difference can be traced to its file.
set -euo pipefail
cd "$(dirname "$0")/../build"
mkdir -p out/digest

# One line per entry: type, mode, path, then the symlink target or the
# content's SHA-256.
list() {
    (
        cd "$1"
        find . -printf '%y %m %p\0' | LC_ALL=C sort -z | while IFS= read -r -d '' entry; do
            path=${entry#* * }
            case $entry in
                l*) printf '%s -> %s\n' "$entry" "$(readlink "$path")" ;;
                f*) printf '%s %s\n' "$entry" "$(sha256sum < "$path" | cut -c1-64)" ;;
                *) printf '%s\n' "$entry" ;;
            esac
        done
    )
}

tree() {
    list "$1" > "out/digest/$2.txt"
    sha256sum < "out/digest/$2.txt" | cut -c1-16
}

file() {
    sha256sum < "$1" | cut -c1-16
}

echo "unsigned parts of this build (ci/digest.sh, lists in build/out/digest/):"
echo "  modules        $(tree out/modules modules)"
echo "  initrd         $(file out/initrd.cpio.zst)"
echo "  initrd tree    $(tree work/initrd-tree initrd-tree)"
echo "  initrd modules $(tree work/initrd-modules initrd-modules)"
echo "  /usr tree      $(tree work/variant-base/usr-tree usr-tree)"
echo "  /usr image     $(file out/base/usr.erofs)"
