# Beize auf echter Hardware

## Rechner des Besitzers

i7-13700K (Raptor Lake, x86-64-v3, kein AVX-512), ASUS ROG STRIX Z790-F GAMING WIFI
(BIOS 3201; Intel 2.5GbE, Wi-Fi 6E + BT, Realtek-USB-Audio), RTX 3070 Ti, 32 GB RAM,
Monitor 1440p @ 240 Hz. Beize liegt auf der Samsung SSD 860 EVO (Disk 0 in Windows);
SanDisk 480 GB (E:, Daten) und Corsair (Windows) nie anfassen. Installieren:
[`keys/secureboot/README.md`](../keys/secureboot/README.md).

Treiber im Image (`bz hw-check`, Stand 2026-10-15): alle PCI-Geräte außer WLAN, Ton
(HDA) und der NVIDIA-GPU – sie kommen mit ihren Phasen; SPI-Flash (BIOS) und Shared
SRAM brauchen keinen.

## Update vom USB-Stick (ADR 0020)

Bestätigt 2026-10-12 (0.1 → 0.2). Alle `bz`-Befehle im eigenen WSL-Terminal (1Password);
`<neu>`/`<alt>` = neue/installierte Version.

1. `OS_VERSION` in `tools/bz/src/layout.rs` auf `"<neu>"` (Commit). Für diesen Commit muss
   ein **voller** CI-Lauf grün sein, nachts oder per Hand (GitHub → Actions → CI → „Run
   workflow“). Dann `./bz build && ./bz release` → `build/out/desktop/release/<neu>/` (6 Dateien, davon die
   SBOM).
2. USB-Stick (FAT32 oder exFAT) in Windows **BEIZE** benennen und den Ordner
   `\\wsl.localhost\Ubuntu-26.04\home\joe\beize\build\out\desktop\release\<neu>` als
   `beize-<neu>` daraufkopieren.
3. Beize starten, Stick einstecken, anmelden:
   ```sh
   run0 mkdir -p /run/stick
   run0 mount -o ro /dev/disk/by-label/BEIZE /run/stick
   run0 beize-update install /run/stick/beize-<neu>
   run0 umount /run/stick
   ```
   Erwartet: „Beize <neu> is installed. It starts with the next restart.“
4. Neu starten (`F8` → Samsung). Erwartet: **ohne** Recovery-Key, gleiches Passwort, Daten
   unverändert; `(. /usr/lib/os-release; echo $IMAGE_VERSION $VARIANT_ID)` → `<neu> desktop`;
   `run0 ls /boot/EFI/Linux` → `beize_<neu>.efi` (gelungen) und `beize_<alt>.efi`;
   `systemctl --failed` → 0 Units. Leertaste beim Start zeigt beide Versionen im Menü.

Weicht etwas ab: Foto vom Bildschirm, oder das Journal wie unter „Startzeit messen“ holen.
Den Rückfall nach drei Fehlstarts prüft der Smoke-Test.

## Desktop testen (M3, Schritt 7: Messbau 0.5.95)

Prüft auf dem echten Rechner, was die VM nicht kann: NVK, den Bildschirmmodus (1440p @ 240 Hz
mit dem EDID-Fix), Maus, Tastatur, den Bildtakt und ob die Sandbox des Compositors mit NVK
hält. Messbau = Zweig `measure/desktop`: `main` + EDID-Patch, Version 0.5.95; ersetzt 0.5.94,
0.6 ersetzt ihn. Gebaut und geprüft (Smoke, Skript in der VM) ist alles, du signierst nur.

**A. Vorbereiten (WSL, dein eigenes Terminal, 1Password entsperrt)**

1. `cd ~/beize && git switch measure/desktop && ./bz build && ./bz release`
   Erwartet: kaum Bauzeit, dann `build/out/desktop/release/0.5.95/` (6 Dateien).
2. `git switch main` (zurück; WSL bleibt auf `main`).
3. In Windows auf den Stick **BEIZE** kopieren (alte Ordner dürfen bleiben):
   - `\\wsl.localhost\Ubuntu-26.04\home\joe\beize\build\out\desktop\release\0.5.95` als
     `beize-0.5.95`
   - `\\wsl.localhost\Ubuntu-26.04\home\joe\hw-test\beize-test` als `beize-test`
     (Skript `hw-test.sh` und `beize-testclient`)

**B. Update installieren (Beize, tty1, als du angemeldet)**

4. Stick einstecken:
   ```sh
   run0 mkdir -p /run/stick
   run0 mount -o ro /dev/disk/by-label/BEIZE /run/stick
   run0 beize-update install /run/stick/beize-0.5.95
   run0 umount /run/stick
   systemctl reboot
   ```
   Erwartet: „Beize 0.5.95 is installed. It starts with the next restart.“, dann Neustart
   (F8 → Samsung) ohne Recovery-Key. Danach `(. /usr/lib/os-release; echo $IMAGE_VERSION)`
   → `0.5.95`. **Kein Bild beim Start:** neu starten, Leertaste halten, 0.5.94 wählen, mir
   Bescheid geben.

**C. Desktop testen**

5. Auf tty1 anmelden und `beize-session` eingeben. Erwartet: nach etwa einer Sekunde ein
   dunkel blaugrauer Bildschirm mit Mauszeiger, sonst nichts (Fenster, Dock und Terminal
   kommen mit Beize UI und der Shell). Maus bewegen: folgt der Zeiger flüssig?
   - Meldung `beize-session: …` und zurück an der Shell: abfotografieren, weiter mit 8
     (`logs`).
   - Schwarz oder eingefroren, Strg+Alt+F2 hilft nicht: Reset-Taste. Nichts geht verloren,
     der Desktop startet nie von selbst; nach dem Neustart weiter mit 8 (`-1`).
6. Strg+Alt+F2, auf tty2 anmelden, Stick **beschreibbar** einhängen und das Skript starten:
   ```sh
   run0 mkdir -p /run/stick
   run0 mount -o uid=$(id -u),gid=$(id -g) /dev/disk/by-label/BEIZE /run/stick
   bash /run/stick/beize-test/hw-test.sh
   ```
7. Sagt das Skript „JETZT Strg+Alt+F1“: wechseln. Nach 10 s füllt das Testfenster den
   Bildschirm: oben rot und grün, unten blau und weiß, durch den oberen Rand läuft ein
   dunkler Balken. Läuft er gleichmäßig, ohne Ruckler oder Risse? Dann a, s, d tippen, die
   Maus über das Fenster bewegen, einmal links klicken. Nach 20 s ist das Fenster weg.
   Strg+Alt+F2: das Skript fragt einmal dein Passwort (run0, für das Journal des Kernels)
   und zeigt das Ergebnis. Erwartet:
   - `GPU /dev/dri/card… (nouveau)`, `renderer on NVIDIA GeForce RTX 3070 Ti (NVK …)`,
     `screen DP-…: 2560x1440 at 240 Hz`
   - `frames: … 4.17 ms apart on average` (240 Hz; 6.94 wären 144 Hz)
   - `Tasten im Fenster: 6` (drücken und loslassen), `Klicks: 2`
   - Warnungen: nur „Unable to become drm master …“ (bekannt, Smithay)
8. Ging etwas schief, statt 7: anmelden, Stick wie in 6 einhängen, dann
   `bash /run/stick/beize-test/hw-test.sh logs` (Meldung von beize-session, derselbe Start)
   oder `bash /run/stick/beize-test/hw-test.sh -1` (nach Reset: die Logs des Starts davor).
   Weiter mit 9.
9. Ende: `run0 umount /run/stick`, `systemctl reboot` (beendet auch den Desktop). In Windows
   den Ordner `beize-test\results-…` vom Stick nach
   `\\wsl.localhost\Ubuntu-26.04\home\joe\hw-logs\` kopieren. Mir sagen: „Hardware-Test
   fertig“ und was du gesehen hast (flüssig? Farben richtig? Ruckler, Risse, Flackern?).

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

Gewöhnlicher Start nach Phase 7 (Beize 0.4, Kernel 7.2.10 mit Beizes Toolchain,
2026-10-16), ohne Bootmenü von systemd-boot; daneben Phase 4 und das Image vor dem
Fahrplan. VM-Werte: Kommentar zu `BOOT_BUDGET` in `tools/bz/src/vm/checks.rs`.

| Phase | nach Phase 7 | nach Phase 4 | vor dem Fahrplan |
| --- | --- | --- | --- |
| Firmware | 15,5 s (0.3) | 21,8 s* | 16,9–17,5 s* |
| Loader (systemd-boot) | 0,54 s | 0,54 s | 1,0–1,1 s |
| Kernel | 7,2 s | 7,2 s | 11,7 s |
| initrd | 4,1 s | 4,0 s | 4,0 s |
| System | 1,7 s | 1,7 s | 1,6–1,7 s |
| ab Loader | 13,6 s | 13,5 s | 18,4 s |

\* Mit der Zeit im F8-Bootmenü (0.4 über F8: 18,4–18,7 s). Ohne Menü starten: in
Windows (Administrator) `bcdedit /enum firmware`, den Eintrag der Samsung-SSD suchen,
dann `bcdedit /set {fwbootmgr} bootsequence {<ID>}` und neu starten – nur der nächste
Start geht zu Beize.

Phase 7 hielt die Zeiten trotz neuer Toolchain, dbus-broker und MiniDebugInfo. Die initrd
(10 MiB) lädt der Loader in 0,5 s; ihre Phase steckt fast ganz in den drei Punkten unten,
nicht in der Größe.

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

Konsole: Auch nach dem Bootmenü von systemd-boot läuft sie mit 160x45, also in der
nativen Auflösung des Monitors (`console-mode max`, ADR 0024). 0.3 fiel dort auf 1024×768 zurück.

Leerlauf (0.4, angemeldet auf tty1; Werte nach 2 min und nach 3 h): Die CPUs sind zu 99,99 %
untätig, die Last liegt bei 0,00. Es wächst nichts, belegt sind 776 → 747 MB. Davon sind
Prozesse nur ein kleiner Teil: Systemdienste 117 MB (udevd 24, journald 12, ntpd-rs 10,
dbus-broker 6), PID 1 35 MB, die Anmeldung 12 MB. Anonymer Speicher aller Prozesse: 26 MB.
Der Rest gehört dem Kernel (Slab 130 MB, davon 110 MB nicht rückgewinnbar; tmpfs 55 MB)
und seinen Reserven, die `MemAvailable` abzieht. „Belegt“ = `MemTotal − MemAvailable`
(Spalte `used` von `free`), je Dienst `systemd-cgtop -m`.

Wird nach jeder Phase des Fahrplans ersetzt, nicht ergänzt.

## Firmware (BIOS) einstellen

Die Firmware ist der größte Posten (15,5 s ohne Menü, siehe oben). Je
Einstellung einzeln ändern und ohne F8 messen (`bz boot-times`), damit klar ist, was
wirkt. Keine davon berührt
PCR 7 (Secure-Boot-Zustand), der Recovery-Key wird also nicht verlangt.

Stand 2026-10-11: alle gesetzt.

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
