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

## Unter Beobachtung: Grafik-Timeouts unter Windows

Standbild, einmal Flackern, wieder da, vor allem beim häufigen Umschalten des YouTube-Vollbilds
(Besitzer 30.10.). Belegt als echte Timeouts (TDR): `nvlddmkm` 153 am 01.10. einmal, am 30.10.
21:29–21:48 fünfmal, dazu Live-Dumps 0x141 (21:29, 21:48; `dwm`, `msedge`). Keine WHEA-Fehler,
kein Bluescreen. NVIDIA 616.92 und Windows 26H2 seit 30.09. **Beize nicht entlastet:** die
Meldung vom 01.10. liegt vor Beizes erstem Hardware-Start (09.10.), aber alle vom 30.10. kamen in
der Sitzung direkt nach 0.5.120 (Start 21:27), nach 27 früheren Messläufen keine: 21:29:43 (vor
WSL, das erst 21:31 startete), 21:31, 21:46, 21:48 (2×), 22:23:52–57 (167×, beim Start von YouTube,
Build und Smoke in WSL liefen). Build um 21:56 und WSL-Starts 22:02–22:12 ohne Meldung: WSL ist
höchstens Auslöser. Nach dem Ausschalten: Start 31.10. 08:25, keine neue Meldung (Stand 08:49);
der Besitzer installiert einen neuen NVIDIA-Treiber. Klären: kalt ohne
Beize und direkt nach einem Neustart aus Beize je 30-mal umschalten, mehrmals, dann
`nvlddmkm` 153 zählen; nur nach Beize → Messbauten pausieren, Warmstart untersuchen. Nach jedem
Messbau nachsehen. Lesen, in PowerShell als Administrator:
`Get-WinEvent -FilterHashtable @{LogName='System';ProviderName='nvlddmkm';StartTime=(Get-Date).AddDays(-7)}`.

Prüfung 30.10. ohne Fund: Beize steuert Lüfter, Takt und Spannung nicht (GSP-Firmware), schreibt
nicht auf BIOS, VBIOS oder SSD-Firmware; 0 MCE/AER in 28 Messläufen. Nicht gemessen: GPU-Temperatur
und -Lüfter unter nouveau (nicht lesbar). Der einmalige Lüfterausfall (erst nach Stromtrennung
weg) gilt dem Besitzer als sporadisch; käme er wieder: LEDs fotografieren, zuerst Netzteil prüfen.
Vor 0.6 muss der IRQ-Fix `kernel/measure/0002-*` auf `main`.

## Desktop testen (Messbau)

Prüft auf dem echten Rechner, was die VM nicht kann: NVK, 1440p @ 240 Hz, den **Zeitplan**
des Compositors, den **Lasttest**, Shell, Glas, das **Terminal** und die Schrift von Beize UI
auf dem Monitor. Messbau =
Zweig `measure/desktop`: `main` + `kernel/measure/*.patch` (nouveau-Patches, die upstream
noch fehlen) + `measure/hw-test.sh`, Version `0.5.<n>` (`OS_VERSION` auf dem Zweig); 0.6
von `main` ersetzt ihn. Claude baut und prüft ihn (Smoke, Skript in der VM) und taggt ihn
als `measure/desktop-0.5.<n>`; der Besitzer signiert nur. Ergebnisse: ADR 0027 §6, Logs in
`~/hw-logs/results-*`. Zuletzt 0.5.103 (25.10.): Testfenster 3 von 4803 Bildern spät, Lasttest
223 von 9381; Befund (Terminal stirbt beim Resizen) in CLAUDE.md §11, behoben in 0.5.104.

**A. Vorbereiten (WSL, dein eigenes Terminal, 1Password entsperrt)**

1. `cd ~/beize && git switch measure/desktop && ./bz build && ./bz release`
   Erwartet: kaum Bauzeit, dann `build/out/desktop/release/0.5.<n>/` (6 Dateien).
2. `git switch main` (zurück; WSL bleibt auf `main`).
3. Claude sagen „Release fertig“: Claude kopiert es als `D:\beize-0.5.<n>` und das Skript
   samt Testprogrammen als `D:\beize-test` auf den Stick **BEIZE**.

**B. Update installieren (Beize, tty1, als du angemeldet)**

4. ```sh
   run0 mkdir -p /run/stick
   run0 mount -o ro /dev/disk/by-label/BEIZE /run/stick
   run0 beize-update install /run/stick/beize-0.5.<n>
   run0 umount /run/stick
   systemctl reboot
   ```
   Erwartet: „Beize 0.5.<n> is installed. It starts with the next restart.“, dann Neustart
   (F8 → Samsung) ohne Recovery-Key. **Kein Bild beim Start:** neu starten, Leertaste
   halten, die vorige Version wählen, Claude Bescheid geben.

**C. Desktop und Terminal (alles im Terminal, kein Wechsel nach tty2 mehr)**

5. Auf tty1 anmelden, `beize-session` eingeben. Erwartet: kurz schwarz, dann blendet der
   Desktop als Ganzes ein – blauer Hintergrund mit Bändern, oben die Menüleiste ohne
   Hintergrund (Uhr rechts), unten das Dock auf Glas mit dem Terminal-Symbol. Es öffnet sich
   **kein** Fenster (wie macOS). **Strg+Alt+T** (oder Klick aufs Terminal im Dock): ein
   Terminal mit Titelleiste und drei Knöpfen, `beize ~ $`. `ls` tippen: kommt die Ausgabe?
   Wirkt die weiße Schrift der Menüleiste scharf, nicht zu dick?
   - Meldung `beize-session: …` und zurück an der Shell: abfotografieren, weiter mit 9.
   - Schwarz oder eingefroren, Strg+Alt+F2 hilft nicht: Reset-Taste. Nichts geht verloren,
     der Desktop startet nie von selbst; nach dem Neustart weiter mit 9 (`-1`).
6. Im Terminal den Stick **beschreibbar** einhängen und das Skript starten (run0 fragt dein
   Passwort hier im Terminal):
   ```sh
   run0 mkdir -p /run/stick
   run0 mount -o uid=$(id -u),gid=$(id -g) /dev/disk/by-label/BEIZE /run/stick
   bash /run/stick/beize-test/hw-test.sh
   ```
7. Das Skript führt durch sechs Teile, jeweils weiter mit Enter:
   1. **Terminal-Probe.** Erwartet, Zeile für Zeile: „Grüße aus Beize: äöü ÄÖÜ ß é ñ €
      „Zitat“ – é“; „fett“ (kräftiger), „schwach“ (grauer), „unterstrichen“,
      „durchgestrichen“, „invers“ (dunkel auf hell); die Wörter rot, grün, gelb, blau,
      magenta, cyan in ihren Farben; zwei Reihen mit je 8 Farbfeldern (dunkel, dann hell);
      ein Grauverlauf schwarz → weiß; ein Farbverlauf türkis → magenta; drei Kästen (dünn
      mit runden Ecken und Trennstrich, dick, doppelt) mit „Ab Cd“, „Ef“, „Gh“, daneben
      Schattierungen und Halbblöcke. Linien der Kästen ohne Lücken? Schrift scharf, nicht
      pixelig?
   2. **Beize UI**, 30 Sekunden ein Fenster mit Text in Inter und JetBrains Mono, hell auf
      dunkel und dunkel auf hell. Wirken heller und dunkler Text gleich kräftig? Danach
      schließt es sich von selbst.
   3. **Bildtakt**, 20 Sekunden das Testfenster: oben rot und grün, unten blau und weiß, durch
      den oberen Rand läuft ein dunkler Balken. Gleichmäßig, ohne Ruckler oder Risse? Dabei
      a, s, d tippen, die Maus über das Fenster bewegen, einmal links klicken.
   4. **Lasttest** (ADR 0028 §9), 40 Sekunden zwölf Fenster mit wandernden Farbverläufen und
      gleitenden Blöcken. Gleich zu Beginn das vorderste an seiner Titelleiste nach unten
      ziehen, bis es zur Hälfte hinter dem Dock liegt, dann die Maus ruhen lassen. Läuft alles
      flüssig? Scheinen die Farben verschwommen und bewegt durchs Dock?
   5. **Fenstergröße**: mit Strg+Alt+T ein zweites Terminal öffnen und es 20 Sekunden lang an
      der rechten unteren Ecke immer wieder ganz groß und klein ziehen. Bleibt es offen?
      Bleibt der Prompt eine saubere Zeile `beize ~ $` (in 0.5.104 vervielfachte er sich)?
      Eine Zeile länger als das Fenster tippen: bricht sie um (bis 0.5.109 schob sie mit `<`
      nach links), bleibt nach Pfeil hoch/runter kein Rest stehen? Danach `exit` darin.
   6. **Bildschirm aus** (ADR 0029 §7, seit 0.5.111): Maus und Tastatur nicht mehr anfassen.
      Nach 10 Minuten blendet das Bild 10 Sekunden ab, dann geht der Monitor in Standby.
      Etwa eine Minute warten, dann die Maus bewegen: kommt das Bild nach wenigen Sekunden
      wieder, wie es war? Bleibt es schwarz: Strg+Alt+F2, dann Strg+Alt+F1; hilft das nicht,
      Reset und danach `hw-test.sh -1`. Das Skript wartet selbst (höchstens 20 Minuten).
      Fotos (Super+Shift+3, jederzeit) legt das Skript zu den Ergebnissen.

   Dann fragt run0 noch einmal dein Passwort (Journal des Kernels), und das Ergebnis steht da.
   Erwartet:
   - `GPU /dev/dri/card… (nouveau)`, `renderer on NVIDIA GeForce RTX 3070 Ti (NVK …)`,
     `screen DP-…: 2560x1440 at 240 Hz, it had …` (der Modus der Konsole davor),
     `rendering with … straight into the screen buffer`, `desktop shown with the shell, … ms`
   - `frames: 4800 of 4800, 4.17 ms apart on average`
   - unter „Bildtakt des Testfensters“: **`pacing: … 0 late (drawing 0, GPU 0, display 0)`**
     und **`change to screen median` höchstens ein Bild** (4,17 ms) – der Prüfwert für den
     Zeitplan; `ready median … before the vblank begins, least …` über 0 (der Spielraum);
     `busy` nur wenige Prozent. Sind doch Bilder spät, sagt die Klammer, welcher Teil des
     Modells nicht hält, und je spätem Bild eine Zeile `late frame (…)` seine Zeiten.
   - unter „Lasttest“: die `pacing:`-Zeilen der Szene, Ziel **`0 late`** und **`GPU median`
     unter 1 ms**; `load: 12 windows, … frames in 40 s` – flüssig sind 12 × 240 × 40 ≈
     115 000 Bilder.
   - unter „Puffer des Compositors“: `held: … dmabufs of clients, … textures` bleibt nach
     dem Ziehen so klein wie davor (in 0.5.103 wuchs es mit jeder Größe, bis der
     Grafikspeicher voll war)
   - `Tasten im Fenster: 7` (das Loslassen der Enter-Taste, die das Fenster startete, gehört
     ihm, wie Wayland es will), `Klicks: 2`
   - unter „Bildschirm aus“: `no input for 600 s: the screen fades to black`, gut 10 s später
     `the screen is off`, nach der Maus `input: the screen is on again`; in den Zeilen der
     Anschlüsse danach `…-DP-…/dpms:On`
   - Warnungen: nur „Unable to become drm master …“ und nach dem Wiedereinschalten „Failed to
     destroy old mode property blob …“ (beide bekannt, Smithay)
8. Noch von Hand (je kurz, Foto nur bei Fehlern):
   - **Fenster:** das Terminal an der Titelleiste ziehen, am Rand größer ziehen, gelber Knopf
     (ins Dock, Klick aufs Symbol holt es zurück), **Super+F** (Vollbild; Maus an den oberen
     Rand holt die Menüleiste auf Glas), noch einmal Super+F.
   - **Strg+Alt+T**: ein zweites Terminal öffnet sich darüber. Darin `exit`: es schließt sich,
     das erste ist wieder da und nimmt Tasten an.
   - **Kopieren:** mit der Maus Text markieren (Doppelklick = Wort), **Strg+Shift+C**,
     dann **Strg+Shift+V**: der Text erscheint am Prompt (mit Strg+U wieder löschen).
   - **Mausrad:** `seq 300` eingeben, dann das Rad nach oben drehen: die Zahlen laufen
     zurück bis 1; nach unten wieder bis 300.
   - **Tote Taste:** `^` und dann `e` ergibt ê am Prompt, `^` und dann Shift+`o` ergibt Ô.
     Mit Strg+U wieder löschen.
9. Ging etwas schief, statt 6–8: Strg+Alt+F2, auf tty2 anmelden, Stick wie in 6 einhängen,
   dann `bash /run/stick/beize-test/hw-test.sh logs` (derselbe Start) oder
   `bash /run/stick/beize-test/hw-test.sh -1` (nach Reset: die Logs des Starts davor).
10. Ende: `run0 umount /run/stick`, `systemctl reboot` (beendet auch den Desktop). Den Stick
    stecken lassen und Claude sagen: „Hardware-Test fertig“, dazu was du gesehen hast
    (Desktop als Ganzes eingeblendet? Schrift der Menüleiste? Glas und Hintergrund in echt?
    Terminal-Probe richtig? Heller und dunkler Text gleich kräftig? Testfenster und Lasttest
    flüssig? Fenster, Vollbild, Strg+Alt+T, Kopieren, Mausrad, ê?). Die Ergebnisse holt Claude
    selbst vom Stick.

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
