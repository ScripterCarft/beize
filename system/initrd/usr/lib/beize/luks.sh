# Shared by Beize's initrd scripts (sourced). The initrd has bash and systemd,
# but no coreutils: builtins only.

# Prints the LUKS volume below the device-mapper device "root".
root_luks_device() {
    local name slave device=
    for name in /sys/block/dm-*/dm/name; do
        [[ -r $name && $(< "$name") == root ]] || continue
        for slave in "${name%/dm/name}"/slaves/*; do
            device=/dev/${slave##*/}
        done
    done
    [[ -b $device ]] || { echo "no LUKS volume below /dev/mapper/root" >&2; return 1; }
    echo "$device"
}

