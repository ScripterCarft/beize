# Package patches

Beize's own fixes for a package other than the kernel, sent upstream (the
owner sends them, `~/upstream/`), carried until upstream has them. Nothing from
third parties. The kernel's lie in `kernel/patches`, under the same rules.

- `recipes/patches/<package>/`, one patch per fix, as sent (`git format-patch`),
  applied in name order to the unpacked source before the build.
- They are part of the package's fingerprint: a changed patch builds it anew,
  from a freshly unpacked source.
- `bz` checks each changed the source. A patch that is already in a new
  release stops the build: upstream took it, and it is removed here in the same
  commit as the update.

| Package | Patch | What | Sent |
| --- | --- | --- | --- |
| systemd | 0001 | logind elects the user's display again when a session changes its type: beize-session's `SetType("wayland")` makes tty1 the display, where polkit finds the shell and its agent (ADR 0031), also after an earlier login on another console | 16.11. by the owner, [systemd#44842](https://github.com/systemd/systemd/pull/44842) |
| systemd | 0002 | run0 waits for the user's password without a timeout: the call that polkit holds while it asks used sd-bus' 25 s, after which run0 failed and polkitd cancelled the dialog (0.5.130 on hardware) | not yet (`~/upstream/systemd-run0-timeout/`) |
