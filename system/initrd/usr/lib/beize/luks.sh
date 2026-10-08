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

# Succeeds if the firmware booted with Secure Boot enforced. The EFI variable
# holds 4 attribute bytes, then 1 for enabled; bash drops the NUL bytes.
secure_boot_on() {
    local var=/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c value
    [[ -r $var ]] || return 1
    { value=$(< "$var"); } 2>/dev/null
    [[ $value == *$'\x01' ]]
}
