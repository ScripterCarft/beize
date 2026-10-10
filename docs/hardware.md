# Beize auf echter Hardware

## Rechner des Besitzers

i7-13700K (Raptor Lake, x86-64-v3, kein AVX-512), ASUS ROG STRIX Z790-F GAMING WIFI
(BIOS 3201; Intel 2.5GbE, Wi-Fi 6E + BT, Realtek-USB-Audio), RTX 3070 Ti, 32 GB RAM,
Monitor 1440p @ 240 Hz. Beize liegt auf der Samsung SSD 860 EVO (Disk 0 in Windows);
SanDisk 480 GB (E:, Daten) und Corsair (Windows) nie anfassen. Installieren:
[`keys/secureboot/README.md`](../keys/secureboot/README.md).

Treiber im Image (`bz hw-check`, Stand 2026-10-10): alle PCI-Geräte außer WLAN, Ton
(HDA) und der NVIDIA-GPU – sie kommen mit ihren Phasen; SPI-Flash (BIOS) und Shared
SRAM brauchen keinen.

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
./bz hw-check ~/hw-logs/journal                 # welches PCI-Gerät im Image einen Treiber hat
```

`bz boot-times` zeigt die Phasen wie systemds „Startup finished“, Pausen zwischen
Kernel-Meldungen (Zeit aus `_SOURCE_MONOTONIC_TIMESTAMP` – der Eintrags-Zeitstempel zeigt
nur, wann journald den Kernel-Puffer las), die langsamsten Units in initrd und System
und die kritische Kette, die `beize-boot-chain.service` nach jedem Start ins Journal
schreibt (fehlt bei Images, die vor dieser Unit gebaut wurden).

## Messwerte

Gewöhnlicher Start (nicht der erste) nach Phase 4 des Fahrplans (Kernel 7.2.9, Image
`18b89a3`, 2026-10-11), daneben Phase 3 und das Image vor dem Fahrplan. VM-Werte:
Kommentar zu `BOOT_BUDGET` in `tools/bz/src/vm/checks.rs`.

| Phase | nach Phase 4 | nach Phase 3 | vor dem Fahrplan |
| --- | --- | --- | --- |
| Firmware | 21,8 s* | 18,5 s* | 16,9–17,5 s* |
| Loader (systemd-boot) | 0,54 s | 0,65 s | 1,0–1,1 s |
| Kernel | 7,2 s | 7,3 s | 11,7 s |
| initrd | 4,0 s | 4,0 s | 4,0 s |
| System | 1,7 s | 1,7 s | 1,6–1,7 s |
| ab Loader | 13,5 s | 13,7 s | 18,4 s |

\* Enthält die Zeit im F8-Bootmenü, also nicht vergleichbar. Für einen echten
Firmware-Wert Beize einmal ohne Menü starten: in Windows (Administrator)
`bcdedit /enum firmware`, den Eintrag der Samsung-SSD suchen, dann
`bcdedit /set {fwbootmgr} bootsequence {<ID>}` und neu starten – nur der nächste Start
geht zu Beize.

Phase 4 verkleinerte die initrd von 19,3 auf 10,7 MiB: Der Loader lädt 0,1 s schneller.
Die initrd-Phase bleibt bei 4,0 s, weil ihre Zeit fast ganz in den drei Punkten unten
steckt, nicht in der Größe.

Kernel: 5,0 s PCIe-Retraining der leeren Root-Ports `00:1a/1b/1c/1d` (je 1 s, Fix in
7.3), 1,9 s `mem auto-init` (`INIT_ON_FREE`, bewusst behalten, ADR 0015). initrd:
- 1,7 s wartet das Stoppen von udevd vor switch-root auf einen Worker, der `product`
  eines USB-Hubs liest. Der Kernel nimmt dafür die Gerätesperre, die der Hub hält,
  solange er alle Ports durchzählt. Upstream-Patch: WSL
  `~/upstream/0002-usb-core-sysfs-string-attrs-without-device-lock.patch`; in der VM
  mit 8 Hubs à 8 Geräten 2,6 s → 11 ms.
- 0,9 s `systemd-cryptsetup@root` (TPM-Entsperren auf Intel PTT).
- 0,7 s `tpm2-setup-early`: Initialisieren der NvPCRs, ~0,2 s je NvPCR, upstream
  bewusst vor dem Entsperren. `hardware`, `verity` und `cryptsetup` sind angelegt,
  `login` passt nicht mehr in den NV-Speicher des TPM (Intel PTT „ADL“, Firmware
  600.18, Spezifikation 1.38; TPM_RC_NV_SPACE). systemd überspringt ihn ordnungsgemäß;
  Beize nutzt ihn nicht (er misst Anmeldungen für spätere Richtlinien). Welche
  fremden NV-Indizes den Platz belegen, zeigt nur `tpm2_getcap handles-nv-index`, das
  Beize nicht mitbringt – kein Anlass, dafür ein Werkzeug ins Image zu nehmen.

Netz: Die DHCP-Adresse kommt 4,6 s (Boot 0) bzw. 12 s (Boot −1) nach dem Link; in der
VM nach 11 ms. Die Verzögerung liegt im Netz (Router prüft die Adresse vor dem Angebot,
oder Spanning Tree am Switch-Port). Sie verzögert den Start nicht (ADR 0011), nur die
erste Zeitsynchronisierung.

Wird nach jeder Phase des Fahrplans ersetzt, nicht ergänzt.

## Firmware (BIOS) einstellen

Die Firmware ist der größte Posten (gemessen bisher nur mit F8-Menü, siehe oben). Je
Einstellung einzeln ändern und ohne F8 messen (`bz boot-times`), damit klar ist, was
wirkt. Keine davon berührt
PCR 7 (Secure-Boot-Zustand), der Recovery-Key wird also nicht verlangt.

| Einstellung (ASUS, BIOS 3201) | Wert | Warum |
| --- | --- | --- |
| Ai Tweaker → DRAM: Memory Context Restore bzw. MRC Fast Boot | Enabled | DDR5 wird sonst bei jedem Start neu trainiert – der größte Posten |
| Boot → Fast Boot | Enabled | überspringt Tests, die nur beim Hardware-Wechsel nötig sind |
| Boot → Next Boot after AC Power Loss | Fast Boot | dasselbe nach Stromausfall |
| Advanced → Network Stack | Disabled | kein Versuch, übers Netz zu starten (PXE) |
| Boot → POST Delay Time | 0 | keine Wartezeit für den Setup-Aufruf |
| Tool → Armoury Crate: Download & Install | Disabled | die WPBT-Tabelle (im ACPI-Log sichtbar) schleust bei jedem Start ein Installationsprogramm in Windows ein; für Beize ohne Nutzen, unter Windows ein Fremdeinfluss auf das System |

Nicht empfohlen: leere PCIe-/M.2-Steckplätze abschalten, um das Retraining zu umgehen
– die Ursache behebt Kernel 7.3. Fast Boot betrifft nur die Firmware; die Tastatur für
den Recovery-Key initialisiert Beize selbst (xHCI und USB-HID fest im Kernel).
