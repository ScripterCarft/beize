# Kernel patches

Beize's own fixes for the kernel, sent upstream (the owner sends them,
`~/upstream/`), carried until upstream has them. Nothing from third parties.

- One patch per fix, as sent (`git format-patch`), applied in name order.
- `bz` applies them before configuring the kernel, and checks each changed the
  source. A patch that is already in a new kernel stops the build: upstream took
  it, and it is removed here in the same commit as the kernel update.
- `kernel/measure/` holds what only a measurement branch carries (logging).

| Patch | What | Sent |
| --- | --- | --- |
| 0001 | nouveau/gsp: EDID blocks 2 and 3 were read as 0 and 1 on GSP-RM; without it no 2560x1440@240Hz on the owner's monitor | by the owner (`~/upstream/nouveau-edid`) |
| 0002 | nouveau/gsp: only interrupts routed to the host are the host's (IRQDEST) | by the owner (`~/upstream/send-nouveau-irq.sh`) |
