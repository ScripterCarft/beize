# Beize – Übergabe für Claude Code

Einstieg für jede neue Session; ersetzt den Chatverlauf. Stand: siehe §11. Was im Code,
in den ADRs, im README oder in `docs/` steht, steht hier nicht noch einmal.

## 1. Was Beize ist

Ein eigenes Betriebssystem auf dem Linux-Kernel: **so konsequent wie macOS, so
frei wie Windows, technisch so offen wie Linux.** Kernel, Treiber und
Basisbibliotheken kommen aus Upstream (selbst gebaut), alles darüber –
Compositor, Shell, Einstellungen, Dienste – wird eigener Code (Rust).
**Lies vor größeren Entscheidungen `docs/VISION.md`.** Meilensteine: M1 Bootet in
QEMU ✓ · **M2 Echte Hardware (in Arbeit)** · M3 Compositor · M4 Alltagstauglich ·
M5 Feinschliff. Stand im Detail: `README.md`.

## 2. Zusammenarbeit mit dem Besitzer

- Kommunikation **auf Deutsch**, verständlich erklären, ehrlich über Schwächen
  („nicht optimiert, bewusst noch nicht“ statt Schönfärberei).
- Anspruch: **„richtig machen statt pfuschen“** – professionell, getestet, Fehler an
  der Wurzel beheben und gegen Wiederkehr absichern (Test, Compile-Time-Check, Guard
  in `bz`). Wenig eigener Code: Upstream + Standardkonfiguration, Eigenes nur mit
  Begründung. Fehlt dem Besitzer Fachwissen: recherchieren, die moderne beste Lösung
  empfehlen und erklären. Doku schlank halten (ersetzen statt anhängen).
- Bei Problemen nicht stur am gewählten Weg festhalten: einen Schritt zurück, Alternativen breit
  abwägen („gehen wir das falsch an?“) und mit Empfehlung vorlegen (Beispiel: homed → eigener Kontodienst).
- Commits und Pushes auf `main` sind freigegeben. Commit-Nachrichten englisch,
  aussagekräftiger Body, Abschluss `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  Ein neuer Signaturschlüssel kommt in einen **eigenen** Commit.
- **1Password ist tabu.** Claude greift nie auf den Tresor zu (kein `op`) und führt
  `bz secureboot new-keys`, `bz sign`, `bz install` nie aus (ADR 0010).
- CI prüft Claude nach jedem Push selbst (`gh run list`/`gh run view`), ohne
  Polling-Schleifen – Actions-Minuten sind knapp.
- Der Besitzer leert den Chat zwischen größeren Schritten: vorher §11 aktuell halten.
- Hardware des Besitzers, Journal von dort auslesen, Messwerte: `docs/hardware.md`.

## 3. Umgebung

- Host Windows 11, Werkbank **WSL2 `Ubuntu-26.04`**, Benutzer `joe`, Repo **`~/beize`**
  (Windows-Pfad `\\wsl.localhost\Ubuntu-26.04\home\joe\beize`). Nicht auf NTFS verschieben.
- GitHub `git@github.com:ScripterCarft/beize.git`. Git in WSL nutzt Windows-`ssh.exe`
  mit dem 1Password-SSH-Agent – **Push hängt, wenn 1Password gesperrt ist**: Besitzer
  bitten zu entsperren. `gh` ist installiert, angemeldet vom Besitzer.
- Claude Code auf Windows (`F:\code-projects`): Befehle als
  `WSL_UTF8=1 wsl.exe -d Ubuntu-26.04 -- bash -l -s <<'EOF' … EOF` (Heredoc; `bash -c`
  wird umgeparst). **Backslashes im Heredoc kommen nicht unverändert an** – `sed`/Python
  mit `\n`, `\\` scheitern still. Solche Änderungen mit Edit/Write über den Windows-Pfad.
  **Edit/Write setzt den Modus auf 644**: Skripte danach in WSL `chmod +x`.

## 4. Architektur

```
Firmware (Secure Boot; PK/KEK/db von Beize + Microsoft im Profil „mit Windows“, ADR 0010;
          im Setup-Modus schreibt systemd-boot sie aus /loader/keys/auto/ selbst ein)
  └─ prüft systemd-boot (signiert, eigene SBAT „beize“)
       └─ prüft UKI beize_<ver>+3.efi = Kernel + initrd + Kommandozeile + signierte
          PCR-11-Richtlinie; Kommandozeile: usrhash=<root hash> mount.usr=/dev/mapper/usr …
            └─ initrd (systemd): beim 1. Start systemd-repart → /usr-Slot B + Root anlegen
               (LUKS2, TPM an PCR 7+12 + signierte PCR-11-Richtlinie „initrd“, Recovery-Key),
               PCR-12-Prüfung, dm-verity für /usr, Root entsperren, switch-root
                 └─ System: /usr (EROFS, dm-verity, read-only) + / (ext4 in LUKS2: /etc, /var, /home)
```

Partitionen: ESP 512 MiB · `/usr` A (1 GiB + 64 MiB verity, Label `beize_<ver>`) ·
`/usr` B (`_empty`, 1. Start) · Root (1. Start, wächst in den Rest). Größen, Typen,
Versionen, repart-Definitionen stehen **nur** in `tools/bz/src/layout.rs`. Nur `/usr`
wird ausgeliefert; `/etc` entsteht beim 1. Start aus `/usr/share/factory/etc` und von
systemd. Boot-Counting: 3 Versuche, `systemd-bless-boot` entfernt den Zähler.
Sicherheitsmodell: ADR 0005 (Quellen), 0008 (verity), 0009 (Daten), 0010 (Schlüssel,
Neubinden, echte Hardware).

## 5. Repository

| Pfad | Inhalt |
| --- | --- |
| `os/` | eigener Rust-Code im Image: `beize-tpm` (initrd: PCR-12-Prüfung, Neubinden nach Recovery, Notfall-Tor mit Neustart einer Version auf Probe, Keymap aus der EFI-Variable), `beize-setup` (1. Start: Tastatur, Konto, Zeitzone), `beize-accounts` (Kontodienst `beize-accountsd`, Konten als systemd-Benutzerdatensätze in `/etc/userdb`; Client-Bibliothek; `beize-account passwd|reset|add`), `beize-update` (ADR 0020: `beize-updated` prüft und installiert über systemd-sysupdate, `beize-update-fetch` holt ohne Rechte, `beize-update install <Quelle>`), `beize-varlink` (Varlink, sd-json, `Secret` für beide Dienste) |
| `tools/bz/src/` | Build-Werkzeug: `source` `recipe` `packages` `install` `kernel` `layout` `tree` `usr` `initrd` `microcode` `secureboot` `image` `installer` `stamp` (Fingerabdrücke der Schritte) `boottimes` `hwcheck` `outdated` `paths` `util` (`run`, `say!`/`capture`); `vm/`: `machine` `console` (`Session`) `report` `checks` `smoke` `run` `debug` |
| `recipes/*.toml` | ein Rezept pro Paket: Quelle (gepinnt + signiert, `latest` für `bz outdated`), `deps`, `stage`, `[build] script`; ohne `stage` nur Quelle (linux, linux-firmware, intel-microcode) |
| `keys/` | Maintainer-Schlüssel + `README.md`; `microsoft/` (gepinnt); `secureboot/` öffentliche Produktionsschlüssel + Anleitung für den Besitzer |
| `images/` | `base.toml` Pakete für `/usr` · `initrd.toml` Funktionen und Module der initrd (ADR 0016, Auswahl in `bz`: `select.rs`) · `base.preset` aktivierte Units · `running-units.txt` was im Smoke-Test laufen muss (ADR 0013) |
| `system/usr/`, `system/initrd/` | eigene Dateien (Units, Boot-Report, Recovery-Key …); in der initrd bash, aber **keine coreutils** |
| `kernel/config/` | Kernel-Fragmente nach Funktion, jede Option begründet, auf `alldefconfig` + Upstream-`hardening.config` (ADR 0015); `kernel/beize.config` = aufgelöste Konfiguration, Änderungen im Diff prüfen und mitcommitten |
| `ci/packages.txt`, `ci/system.sh` | einzige Liste der Build-Abhängigkeiten; kompletter CI-Ablauf |

## 6. Bauen und Testen (in `~/beize`)

```sh
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
./bz build           # kernel → Pakete → usr → initrd → image; jeder Schritt nur, wenn sich seine Eingaben ändern
./bz smoke --quick   # Installation + 2 Starts mit allen Prüfungen (~30 s): die schnelle Schleife
./bz smoke           # DER Abnahmetest (~2 min, Szenarien parallel), vor jedem Commit grün
./bz debug 'journalctl -b -p warning' …   # Befehle auf der Root-Shell einer VM, Ausgabe auf stdout
#   --machine <name> (von smoke liegengelassen) · --tpm <dir> · --recovery-key … · '@wait N'
./bz run             # interaktiv (Ctrl-A X); VM-Disk bleibt bis zum nächsten Image
./bz boot-times ~/hw-logs/journal [--boot -1]   # Phasen, Kernel-Lücken, langsamste Units, Kette
./bz hw-check ~/hw-logs/journal   # welches PCI-Gerät des Rechners im Image einen Treiber hat
./bz outdated        # neuere Upstream-Releases aller Quellen
./bz verify · ./bz package <name> · ./bz owner /usr/bin/bash · ./bz -v …  (alles ausgeben)
# --locked (CI): kernel/beize.config muss aktuell sein, statt neu geschrieben zu werden
# nur der Besitzer, im Terminal, mit 1Password: ./bz secureboot new-keys|new-update-key ·
#   ./bz sign · ./bz release (signiertes Update nach build/out/release/<ver>) · ./bz install
```

- Ausgabe ist knapp: Befehle zeigen nur bei Fehler ihre letzten 40 Zeilen. Logs:
  `build/work/logs/<paket>.log`, `linux.log`, Konsole jedes Starts in
  `build/work/logs/boot/NN-<maschine>.log` (gehört immer zum letzten smoke/debug/run),
  daneben das Journal jedes Starts mit Boot-Report als `.journal.json` (über eine leere virtio-Disk;
  `bz boot-times <datei>`). Smoke zeigt beim Start mit Zeitbudget die langsamsten Units.
  Die Konsolengröße der VM steht in der Kommandozeile (`systemd.tty.*`, sonst fragt PID 1 2× 333 ms).
- Zeiten: nichts geändert 0,1 s; Datei in `system/usr` 1,6 s; Rezeptänderung an systemd
  ~5 min (frisches Build-Verzeichnis, dazu dbus); Bau von null 14,8 min (2026-10-12, davon
  Toolchain 7,7 min), Plattenspitze 7 GiB, danach `build/` 5,2 GB. Die CI baut wöchentlich von null.
- Was `bz smoke` prüft, steht in `tools/bz/src/vm/smoke.rs` (Modul-Doku) und
  `vm/checks.rs`. Ein gescheitertes Szenario lässt seine Maschine liegen
  (`bz debug --machine smoke-db` usw.).
- Start in der VM: 3,7 s Gast-Uptime, erster Start ~6 s, Smoke lokal ~105 s. Der zweite Start
  hat ein Zeitbudget je Phase (`BOOT_BUDGET` in `vm/checks.rs`, ~1,5× CI-Messwert); bei
  Änderung neu messen und den Kommentar aktualisieren.
- CI (`.github/workflows/ci.yml`): fmt/clippy/test bei jedem Push; auf `main`, manuell
  und wöchentlich (dann von null) `ci/system.sh` im privilegierten `ubuntu:26.04`-Container
  (gespiegelt in GHCR, `mirror.yml`) mit KVM. Toolchain, Sysroot, Stamps, Kernel-Objektbaum und Microcode
  im Actions-Cache; `build/out` nicht, daher laufen die Schritte ab Kernel dort immer. Der
  Runner ist klein (Smoke-Szenarien nacheinander, ~14 min). Repo privat: Actions-Minuten sparsam.

## 7. ADRs (`docs/adr/`)

0001 Kernel aus Upstream · 0002 glibc + systemd · 0003 unveränderliches signiertes Image ·
0004 WSL2 · 0005 Cross-Toolchain, Rezepte, signierte Quellen · 0006 Secure Boot, Test-
vs. Produktionsschlüssel · 0007 nur `/usr`, A/B, erster Start · 0008 dm-verity · 0009 Root
mit LUKS2 + TPM · 0010 Plattform-Eigentümer, Profile, Neubinden, echte Hardware · 0011
Netzwerk · 0012 Zeit (NTS) · 0013 Was läuft (Units im Image) · 0014 Toolchain x86-64-v3 + Härtung · 0015 Kernel: Konfiguration, Module, Microcode, Firmware · 0016 initrd aus Funktionen, Notfall-Shell, hwdb · 0017 Tastatur (XKB, kbd-Keymaps, Keymap der initrd in EFI-Variable) · 0018 Rechte (run0 + polkit, kein setuid, Wiederherstellung) · 0019 Konten (Beizes Kontodienst, `/etc/userdb`, Rechte durch Beweis; homed verworfen) · 0020 Updates (beize-updated + systemd-sysupdate, ML-DSA-signierte Liste, 4-GiB-Slots). Neue Architekturentscheidung = neues ADR; spätere Änderungen
in die Endfassung einarbeiten, keine Nachträge.

## 8. Regeln für Rezepte und Code

- Nur `/usr` ausliefern: `bz` bricht ab bei Dateien außerhalb; Altlasten im Rezept
  löschen (mit Kommentar) oder nach `/usr/share/factory` verlegen.
- **pkg-config liefert beim Cross-Build Pfade mit Sysroot-Präfix** (Unit-, udev-,
  tmpfiles-, sysusers-Verzeichnisse): immer explizit setzen.
- `meson -Dauto_features=disabled`: jedes benötigte Feature ausdrücklich einschalten.
- Abhängigkeitsschleifen vermeiden (libdevmapper ohne udev-Sync: systemd → cryptsetup → lvm2).
- **Eigener Rust-Code im Image:** Crate unter `os/<name>`, Rezept mit `repo = [...]`
  nach Vorbild `recipes/beize-tpm.toml`. Crates mit Build-Skript meiden (laufen mit
  dem Host-cc). Krypto nie selbst schreiben.
- Rezepte **ohne** `repo` behalten ihren Fingerprint (Test) – sonst baut alles neu.
- **Ein Paket sieht beim Bauen nur seine (transitiven) `deps` im Sysroot.** Fehlt etwas,
  gehört es in `deps` – nicht die Sperre umgehen.
- Build-Jobs nach RAM begrenzt (1,5 GiB/Job), parallele VMs nach CPUs/RAM (je 4 CPUs
  und 3 GiB reserviert, Gast 2 GiB) – nicht zurückdrehen.
- Werkzeuge, die zur systemd-Version im Image passen müssen (ukify, systemd-measure),
  kommen aus unserem systemd-Build (ADR 0009).
- Host-Werkzeuge, die `bz` aufruft, gehören in `ci/packages.txt`. Im Gast gibt es kein
  `sed`/`awk`/`grep` (Skripte in reinem Bash).
- Autotools-Abfragen beim Host (`AC_CHECK_PROG`) im Rezept beantworten (`ac_cv_…=yes`).
- Skripte unter `system/` müssen ausführbar sein (Test prüft das).
- **Units aktivieren nur über `images/base.preset`** (mit Grund; `bz` legt die Links in
  `/usr`), nie über Presets oder `.wants`-Links in `system/`. Ändert sich, was läuft,
  `images/running-units.txt` mitpflegen (ADR 0013).
- **initrd (ADR 0016):** Was hinein soll, steht als Funktion mit Grund in
  `images/initrd.toml`; `bz` leitet Programme und Bibliotheken ab und bricht bei Lücken
  ab. Nicht ableitbar (deshalb dort ausdrücklich): was Generatoren erzeugen und deren
  Programme, `mount`/`umount` der Mount-Units, Daten (nvpcr, tmpfiles.d). Fehlt etwas,
  zeigt es erst der Smoke-Test.
- Notfallzugang nur über `beize-tpm emergency` (Drop-ins für emergency/rescue), nie
  `sulogin`, `debug-shell` oder ein Generator, der eine Shell öffnet.
- **Tastatur (ADR 0017):** ein Layout immer als ganze Zeile aus systemds
  `kbd-model-map` setzen, deren Keymap es gibt (`localectl … --no-convert`, wie
  beize-setup) – localed rät sonst auch Keymaps, die kbd nicht hat.
- **Kernel-Optionen** nur in das passende Fragment unter `kernel/config/`, mit Grund
  (Vorbild Fedora nennen); fehlende Abhängigkeiten meldet `bz`. Gerätetreiber als
  Modul (`=m`), fest nur Infrastruktur vor udev (ADR 0015).
- Neue Quelle: `latest = { release-monitoring = <id> }` (ID auf release-monitoring.org
  suchen, Homepage prüfen); Test verlangt es.
- Ausgaben in `bz` über `say!` (nicht `eprintln!`), damit parallele Szenarien sie
  gesammelt zeigen; Prüfungen im Smoke-Test müssen belegen, dass ihre Bedingung
  hergestellt wurde (Gegenprobe: Eigenschaft absichtlich brechen → rot).

## 9. Bekannte Fallstricke

- WSL-VM nach OOM instabil → `wsl --shutdown`. JetBrains-Backend: `build/` ausschließen.
- WSL beendet die Distro ohne angehängte `wsl.exe`-Sitzung → lange Jobs im Vordergrund
  oder mit gehaltener Sitzung; Logs nach `~`, nicht `/tmp`.
- `qemu -serial stdio` liest stdin – in Skripten `< /dev/null`.
- **CI rot, lokal grün?** Den Lauf 1:1 nachstellen: `rsync -a --exclude /build --exclude
  /target ~/beize/ ~/ci-x/`, dann `sudo podman run --rm --privileged --device /dev/kvm -v
  ~/ci-x:/w -w /w public.ecr.aws/docker/library/ubuntu:26.04 ci/system.sh`; danach
  `build/work/initrd-tree` bzw. `usr-tree` mit `diff -rq` vergleichen.
- `actions/cache` im Container: `zstd` **vor** dem Wiederherstellen installieren.
- Kernel: `allnoconfig` verneint auch Upstreams Sicherheitsstandards (Mitigations) –
  Basis bleibt `alldefconfig`. Module installiert kbuild nach `INSTALL_MOD_PATH/lib/modules`
  (es gibt kein `KERNEL_MODULE_DIRECTORY`).
- Große Dateien per QEMU-fw_cfg verzögern die Firmware enorm (237 KiB: 45 s) – nur
  kleine Testdateien so hineinreichen.
- FAT32 braucht ≥ 65 525 Cluster (Compile-Time-Check); auf größeren Disks Backup-GPT
  verschieben (`sfdisk --relocate gpt-bak-std`).
- systemd-repart lässt Partitionen ohne `SizeMaxBytes=` wachsen, auch bestehende (Test
  `only_root_grows`). Vorab: `systemd-repart --definitions=… --dry-run=yes <Kopie>`.
- systemd-Konsolenzeilen enthalten ANSI-Codes **mitten im Text** → `strip_ansi`.
- `sbvarsign` signiert die `.auth`-Dateien mit der aktuellen Zeit: `disk.img` ist nie
  bit-gleich (der Image-Stamp nimmt deshalb die Schlüssel, nicht die `.auth`).
- Smoke tippt in der initrd über QMP echte Tasten (Scancodes, die Keymap entscheidet):
  Tasten, bevor die Abfrage tty1 geöffnet hat, gehen verloren – daher `PASSWORD_PAUSE`
  (500 ms) nach dem Prompt auf der seriellen Konsole, auch vor jedem Tippen über QMP.
- Gegenproben nur gezielt zurücksetzen (Backup der Datei), nie `git checkout` auf eine
  Datei mit noch nicht committeten Änderungen.
- Rust `fs::copy` schreibt sparse Dateien voll aus (ext4: über den Page-Cache): Disks immer
  mit `cp --sparse=always` kopieren (`vm::machine::copy_sparse`).
- Rust `Command::output()` setzt stdin auf null: Programme, die am Terminal fragen sollen
  (systemd-ask-password), brauchen `.stdin(Stdio::inherit())`, sonst warten sie auf
  einen Passwort-Agenten.

## 10. Offene Punkte

**GitHub Issues** (`gh issue list`; Labels `phase-7`, `upstream`, `hardware`). Neue Punkte dort
anlegen, nicht hier; ein Commit, der einen Punkt erledigt, nennt `Fixes #N`. Upstream-Entwürfe
liegen in WSL `~/upstream/` (sendet der Besitzer, #13).

## 11. STAND UND NÄCHSTE AUFGABE (2026-10-12)

**Beize läuft auf dem Rechner des Besitzers**, installiert ist 0.2 (`d4ee932`; Disks, Messwerte,
Update-Test: `docs/hardware.md`; neu installieren: `keys/secureboot/README.md`). 0.2 ist noch mit
dem Bootstrap-Compiler gebaut (Fund Phase 7) – beim Hardware-Test am Ende von Phase 7 ersetzen.
Wiederherstellung auf der Hardware: nach F8 Leertaste halten, „(Recovery)“, Recovery-Key, `s`.

**PHASE 7 – Konsolidierung (seit 2026-10-12, Plan mit dem Besitzer abgestimmt).** Ausgangswerte
und Rohdaten: WSL `~/p7/BASELINE.md`. Jede Änderung mit Vorher/Nachher, je Thema ein Commit.
Erledigt (gepusht bis `ca2560d`):
- Bau von null repariert; **seit 2026-10-10 baute gcc-pass1 alle Pakete** (gleiches Präfix) →
  pass1 in `build/tools/pass1`, ELF-Prüfung verlangt Build-IDs; Paket wird vor dem Neubau
  deinstalliert (`install::uninstall`); `bz` räumt `work/` auf (11 → 5,2 GB). Bau von null lokal
  14:48; die CI baut wöchentlich von null (2:11 h).
- CI: Quellen-Cache nach Pins ohne linux-firmware, Microcode in `build/work`, Build-Zustand nach
  Stempeln benannt, verify/Artefakt nur wöchentlich/manuell.
- Beschädigtes `/usr` → Tor nach ~1 s statt 91 s (`99-zz-beize-verity.rules`); Smoke-Frist 60 s;
  Tastatur-Pause gegen einen Flake.
- Kernel zstd (UKI 28,5 → 27,3 MB); initrd -19 gemessen bestätigt; `/usr` zstd 9 + 64 KiB +
  fragments, `LEFT_OUT` in `usr.rs` (mit dem Besitzer), Linker-Namen raus, ELF-Prüfung löst
  NEEDED wie der Lader auf: `/usr` 80,2 → 40,8 MiB. coreutils bleiben Einzelprogramme (gemessen).

**NÄCHSTER CHAT – zuerst:** CI 38305117376 prüfen (Push `66e5bfb`; erstmals mit dem Build-Image aus
GHCR, Mirror-Lauf 38305037919 grün; Fixes #1 – bei Grün #1 schließen), dann diesen lokalen Commit
pushen. Runner belegt: 2 CPUs, 7,8 GiB, 8,4 GB frei; Normallauf ~20 min, Smoke ~850 s. Der `gh`-Token
darf keine Workflows starten (Mirror startet der Besitzer). Speicherbedarf je Paket
(`memory.peak`, enthält Seiten-Cache – für #3 den anonymen Anteil messen): WSL `~/p7/memory-peaks.txt`.

Offen, in dieser Reihenfolge (Issues mit Label `phase-7`):
1. CI: Kosten des Baus von null (#2: 2:11 h auf 2 CPUs) – **kein self-hosted Runner** (Besitzer);
   PRs nur, wenn sie nicht langsamer machen (Cache je Branch).
2. **Pakete parallel bauen (#3) – Besitzer: RAM-gesteuert, nie OOM.**
3. Doku schlank halten: es kommt viel dazu, wenig weg – nur Funktionsloses entfernen (Besitzer).
4. Upstream statt Eigenem nur messen und bewerten (#4); **Go nur bei großem Gewinn ohne
   Einschränkung**. Bit-genaue Builds (#5).
5. Leerlauf (#6), Review (#7), Hardware-Test (#8). `~/beize/build.before` löschen.

**Fahrplan bis M2 (vom Besitzer freigegeben):** je Phase eigener Chat, ADR vorab abstimmen, nichts
abschalten „weil unnötig“, keine fremden Patches – Ursachen sauber lösen; bei Fehlern zuerst
fragen, welche eigene Entscheidung uns auf einen ungetesteten Pfad gebracht hat.
1. ✓ Messen als Werkzeug (`bz boot-times`, Zeitbudget, `docs/hardware.md`); Was läuft (ADR 0013).
2. ✓ Toolchain x86-64-v3 + Härtung (ADR 0014).
3. ✓ Kernel (ADR 0015).
4. ✓ Startpfad, initrd aus Funktionen, Notfall-Tor (ADR 0016).
5. ✓ Grundfunktionen: Tastatur (0017), Rechte (0018), ping, Konten (0019).
6. ✓ A/B-Updates (ADR 0020), auf der Hardware bestätigt (0.1 → 0.2 vom USB-Stick).
7. **Konsolidierung vor M3** (läuft): auf der Hardware messen (Start, Leerlauf über Stunden –
   nichts wächst), Review des eigenen Codes, offene Punkte; Funktionen bleiben, geändert wird
   nur nachweislich Besseres. Nicht feinschleifen, was M3 ersetzt (Konsole, Setup-Oberfläche).
