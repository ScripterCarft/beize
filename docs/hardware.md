# Beize auf echter Hardware

## Rechner des Besitzers

i7-13700K (Raptor Lake, x86-64-v3, kein AVX-512), ASUS ROG STRIX Z790-F GAMING WIFI
(BIOS 3201; Intel 2.5GbE, Wi-Fi 6E + BT, Realtek-USB-Audio), RTX 3070 Ti, 32 GB RAM,
Monitor 1440p @ 240 Hz. Beize liegt auf der Samsung SSD 860 EVO (Disk 0 in Windows);
SanDisk 480 GB (E:, Daten) und Corsair (Windows) nie anfassen. Installieren:
[`keys/secureboot/README.md`](../keys/secureboot/README.md).

## Startzeit messen

Das Journal auf der Root-Partition ist persistent; es lässt sich unter WSL ohne Neubau
lesen. Der Besitzer bindet die Disk ein und entsperrt sie (Recovery-Key, tippt nur er):

```sh
wsl --mount \\.\PHYSICALDRIVE0 --bare           # in Windows (Administrator)
lsblk -o NAME,PARTLABEL                         # Buchstabe wechselt: Partition "root" suchen
sudo cryptsetup open --readonly /dev/sdX6 beize-root
sudo mount -o ro,noload /dev/mapper/beize-root /mnt
sudo cp -r /mnt/var/log/journal ~/hw-logs/ && sudo chown -R "$USER" ~/hw-logs
sudo umount /mnt && sudo cryptsetup close beize-root   # danach: wsl --unmount
```

Auswerten:

```sh
./bz boot-times ~/hw-logs/journal               # letzter Start
./bz boot-times ~/hw-logs/journal --boot -1     # der davor; --gap 500 nur Pausen ab 500 ms
```

`bz boot-times` zeigt die Phasen wie systemds „Startup finished“, Pausen zwischen
Kernel-Meldungen (Zeit aus `_SOURCE_MONOTONIC_TIMESTAMP` – der Eintrags-Zeitstempel zeigt
nur, wann journald den Kernel-Puffer las), die langsamsten Units in initrd und System
und die kritische Kette, die `beize-boot-chain.service` nach jedem Start ins Journal
schreibt (fehlt bei Images, die vor dieser Unit gebaut wurden).

## Messwerte

Stand 2026-10-10, Image vor dem M2-Fahrplan; gewöhnliche Starts (nicht der erste):

| Phase | Hardware | VM (`bz smoke`) |
| --- | --- | --- |
| Firmware | 16,9–17,5 s (erster Start nach Installation 21,8 s) | – |
| Loader (systemd-boot) | 1,0–1,1 s | – |
| Kernel | 11,7 s | 0,56–0,61 s |
| initrd | 4,0 s | 2,42–2,51 s |
| System | 1,6–1,7 s | 2,27–2,44 s |
| Summe | 35,3–36,0 s | – |

Wird nach jeder Phase des Fahrplans ersetzt, nicht ergänzt.
