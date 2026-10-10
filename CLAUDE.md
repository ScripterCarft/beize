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
- Commits und Pushes auf `main` sind freigegeben. Commit-Nachrichten englisch,
  aussagekräftiger Body, Abschluss `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  Ein neuer Signaturschlüssel kommt in einen **eigenen** Commit.
- **1Password ist tabu.** Claude greift nie auf den Tresor zu (kein `op`) und führt
  `bz secureboot new-keys`, `bz sign`, `bz install` nie aus (ADR 0010).
- Die CI beobachtet der Besitzer selbst und meldet, wenn sie rot ist.
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
| `os/` | eigener Rust-Code im Image: `beize-tpm` (initrd: PCR-12-Prüfung, Neubinden nach Recovery), `beize-setup` (1. Start: Konto, Zeitzone) |
| `tools/bz/src/` | Build-Werkzeug: `source` `recipe` `packages` `install` `kernel` `layout` `tree` `usr` `initrd` `secureboot` `image` `installer` `stamp` (Fingerabdrücke der Schritte) `boottimes` `paths` `util` (`run`, `say!`/`capture`); `vm/`: `machine` `console` (`Session`) `report` `checks` `smoke` `run` `debug` |
| `recipes/*.toml` | ein Rezept pro Paket: Quelle (gepinnt + signiert), `deps`, `stage`, `[build] script` |
| `keys/` | Maintainer-Schlüssel + `README.md`; `microsoft/` (gepinnt); `secureboot/` öffentliche Produktionsschlüssel + Anleitung für den Besitzer |
| `images/` | `base.toml` Pakete für `/usr` und initrd · `base.preset` aktivierte Units · `running-units.txt` was im Smoke-Test laufen muss (ADR 0013) |
| `system/usr/`, `system/initrd/` | eigene Dateien (Units, Boot-Report, Recovery-Key …); in der initrd bash, aber **keine coreutils** |
| `kernel/config/` | Kernel-Fragmente (`beize-base`, `beize-hardware`), nach dem Merge verifiziert |
| `ci/packages.txt`, `ci/system.sh` | einzige Liste der Build-Abhängigkeiten; kompletter CI-Ablauf |

## 6. Bauen und Testen (in `~/beize`)

```sh
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
./bz build           # kernel → Pakete → usr → initrd → image; jeder Schritt nur, wenn sich seine Eingaben ändern
./bz smoke --quick   # Installation + 2 Starts mit allen Prüfungen (~30 s): die schnelle Schleife
./bz smoke           # DER Abnahmetest (~75 s, Szenarien parallel), vor jedem Commit grün
./bz debug 'journalctl -b -p warning' …   # Befehle auf der Root-Shell einer VM, Ausgabe auf stdout
#   --machine <name> (von smoke liegengelassen) · --tpm <dir> · --recovery-key … · '@wait N'
./bz run             # interaktiv (Ctrl-A X); VM-Disk bleibt bis zum nächsten Image
./bz boot-times ~/hw-logs/journal [--boot -1]   # Phasen, Kernel-Lücken, langsamste Units, Kette
./bz verify · ./bz package <name> · ./bz owner /usr/bin/bash · ./bz -v …  (alles ausgeben)
# nur der Besitzer, im Terminal, mit 1Password: ./bz secureboot new-keys · ./bz sign · ./bz install
```

- Ausgabe ist knapp: Befehle zeigen nur bei Fehler ihre letzten 40 Zeilen. Logs:
  `build/work/logs/<paket>.log`, `linux.log`, Konsole jedes Starts in
  `build/work/logs/boot/NN-<maschine>.log` (gehört immer zum letzten smoke/debug/run).
- Zeiten: nichts geändert 0,1 s; Datei in `system/usr` 1,6 s; Rezeptänderung an systemd
  ~5 min (frisches Build-Verzeichnis, dazu dbus); voller Neubau ~10–15 min.
- Was `bz smoke` prüft, steht in `tools/bz/src/vm/smoke.rs` (Modul-Doku) und
  `vm/checks.rs`. Ein gescheitertes Szenario lässt seine Maschine liegen
  (`bz debug --machine smoke-db` usw.).
- Start in der VM: 5,3 s Gast-Uptime, erster Start ~8 s. Der zweite Start hat ein
  Zeitbudget je Phase (`BOOT_BUDGET` in `vm/checks.rs`, ~1,5× CI-Messwert); bei Änderung neu
  messen und den Kommentar aktualisieren.
- CI (`.github/workflows/ci.yml`): fmt/clippy/test bei jedem Push; auf `main`, manuell
  und wöchentlich `ci/system.sh` im privilegierten `ubuntu:26.04`-Container
  (`public.ecr.aws`) mit KVM. Pakete, Toolchain, Sysroot, Stamps und Kernel-Objektbaum
  im Actions-Cache (auch nach Abbruch gespeichert); `build/out` nicht, daher laufen die
  Schritte ab Kernel dort immer. Mit Cache ~12–17 min; 4-Kern-Runner → Smoke-Szenarien
  nacheinander. Repo privat: Actions-Minuten sparsam.

## 7. ADRs (`docs/adr/`)

0001 Kernel aus Upstream · 0002 glibc + systemd · 0003 unveränderliches signiertes Image ·
0004 WSL2 · 0005 Cross-Toolchain, Rezepte, signierte Quellen · 0006 Secure Boot, Test-
vs. Produktionsschlüssel · 0007 nur `/usr`, A/B, erster Start · 0008 dm-verity · 0009 Root
mit LUKS2 + TPM · 0010 Plattform-Eigentümer, Profile, Neubinden, echte Hardware · 0011
Netzwerk · 0012 Zeit (NTS) · 0013 Was läuft (Units im Image) · 0014 Toolchain x86-64-v3 + Härtung. Neue Architekturentscheidung = neues ADR; spätere Änderungen
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
- FAT32 braucht ≥ 65 525 Cluster (Compile-Time-Check); auf größeren Disks Backup-GPT
  verschieben (`sfdisk --relocate gpt-bak-std`).
- systemd-repart lässt Partitionen ohne `SizeMaxBytes=` wachsen, auch bestehende (Test
  `only_root_grows`). Vorab: `systemd-repart --definitions=… --dry-run=yes <Kopie>`.
- systemd-Konsolenzeilen enthalten ANSI-Codes **mitten im Text** → `strip_ansi`.
- `sbvarsign` signiert die `.auth`-Dateien mit der aktuellen Zeit: `disk.img` ist nie
  bit-gleich (der Image-Stamp nimmt deshalb die Schlüssel, nicht die `.auth`).

## 10. Offene Punkte (außer der aktuellen Aufgabe)

- initrd 17,9 MiB (zstd -19) – enthält noch ganz systemd; verschlanken. hwdb nicht generiert.
- Updater (systemd-sysupdate), Verhalten bei verity-Fehlern zur Laufzeit, WLAN, NVIDIA,
  Bluetooth, Start von der zweiten SSD, Installer.
- TPM: Rollback älterer signierter UKIs; `CONFIG_TCG_TPM2_HMAC` aus; Attestierung bei
  neuem TPM. Profil „Nur Beize“ (Option-ROM-Hashes, ADR 0010).
- Recovery-Key nur auf der Konsole – das Onboarding (M3+) muss ihn grafisch zeigen.
- Bit-genaue Builds (Container- vs. lokaler Bau unterscheiden sich, vermutlich Build-Pfad).
- systemd-Rezeptänderungen bauen systemd komplett neu (~5 min): bewusst so gelassen
  (frisches Build-Verzeichnis = kein veralteter Zustand; meson-Optionen landen in
  `config.h`, inkrementell würde ohnehin fast alles neu übersetzt).
- CI, zurückgestellt: `permissions: contents: read`, `push` nur auf `main` +
  `pull_request`, Dependabot + Actions auf Commit-SHAs, rustup-Warnung im Container.

## 11. STAND UND NÄCHSTE AUFGABE (2026-10-10)

**Beize läuft auf dem Rechner des Besitzers** (Disks und Messwerte: `docs/hardware.md`;
neu installieren: `keys/secureboot/README.md`). Das installierte Image hat
`beize-boot-chain` noch nicht – die kritische Kette der Hardware gibt es erst nach der
nächsten Installation.

**Nächster Schritt (2026-10-10):** CI-Lauf von `5c577e1` prüfen (`gh run list`; baut die
Toolchain neu, 1–2 h). Die lokalen Commits danach sind noch nicht gepusht – erst nach der
CI pushen (ein Push bricht den laufenden Lauf ab). Dann Phase 3.

**Fahrplan bis M2 (vom Besitzer freigegeben; je Phase eigener Chat, ADR vorab abstimmen,
nichts abschalten „weil unnötig“, keine fremden Patches – Ursachen sauber lösen):**
1. ✓ **Messen als Werkzeug:** `bz boot-times`, `beize-boot-chain.service`, Zeitbudget im
   Smoke-Test, `docs/hardware.md`.
   ✓ **1b. Was läuft (ADR 0013):** Units im signierten Image statt in `/etc`, Regeln
   `images/base.preset` → systemd-Vorgaben → `disable *`; Smoke prüft die laufenden Units.
2. ✓ **Toolchain (ADR 0014):** gcc mit x86-64-v3/`generic` als Standard, Härtung wie Fedora,
   ELF-Prüfung in `bz` (alle ELF in `/usr` + initrd; Ausnahmen `NO_CET` in `elf.rs`), Kernel
   mit Shadow Stacks für Programme, Boot-Report `shstk=` (WSL-VMs ohne CET: `nohw`).
3. **Kernel (ADR 0015):** Gerätetreiber als signierte Module (Wegwerf-Schlüssel, zstd, unter
   verity), udev lädt per Modalias; fest nur Infrastruktur vor udev. Eigene gegliederte
   Fragmente statt defconfig+kvm_guest (dabei PCIe-Dienste wie Fedora: `HOTPLUG_PCI_PCIE`,
   `PCIEAER`, `ACPI_PCI_SLOT` fehlen bisher). kmod, systemd `kmod`/`acl`. Microcode im UKI
   (`.ucode`), linux-firmware nur nach `modinfo -F firmware`. `bz outdated`. Entschieden
   (Besitzer): **neuester Stable**, nicht LTS – neue Funktionen und Geräteunterstützung zuerst.
   Kernel-Wartezeit auf der Hardware (11,7 s, Fedora-Live 0,8 s), „Upstream first“, kein
   Eigen-Patch: ~5 s PCIe-Retraining leerer Root-Ports = Regression aus 7.2 (72780f796468),
   Fix „Skip Target Speed quirk on clamped ports with no link“ liegt upstream auf pci/misc
   für 7.3 → mit 7.3 prüfen. ~5,7 s `mei_me` an 00:14.0 (xHCI, gleiche ID 8086:7a60) –
   tritt nur auf, weil mei_me fest eingebaut ist. Als Modul nur behoben, wenn xHCI fest
   bleibt (wie Fedora; sonst Wettlauf beider Module um dieselbe Modalias) – passt ohnehin,
   die Tastatur für den Recovery-Key braucht xHCI vor udev. Patch für den Besitzer liegt in WSL
   `~/upstream/0001-mei-me-*.patch` (Name/Signed-off-by eintragen, an die Adressen aus
   `get_maintainer.pl` senden). Grundsatz: Bei Fehlern zuerst fragen, welche eigene
   Entscheidung uns auf einen ungetesteten Pfad gebracht hat.
4. **Startpfad:** initrd nach Funktionen + ELF-Abhängigkeiten zusammensetzen, hwdb bauen,
   NvPCR-Fehler von `systemd-tpm2-setup` und NTS-vor-Netz bei ntpd-rs an der Ursache lösen,
   kritische Kette straffen, BIOS-Empfehlungen dokumentieren.
5. **Grundfunktionen:** a) Tastatur/Locale (ADR 0016): XKB als einzige Quelle
   (xkeyboard-config, libxkbcommon, localed, kbd), beize-setup fragt Layout vor dem Passwort.
   b) Rechte (ADR 0017): run0 + polkit (duktape), kein setuid in `/usr` (Test).
   c) iputils `ping` ohne setuid/Capabilities (ping_group_range), Smoke-Check.
6. **A/B-Updates (ADR 0018) → M2:** Slot-Größe vorher neu festlegen (letzte Neuinstallation),
   systemd-sysupdate (usr, verity, UKI, signierter Bootloader – `systemd-boot-update` ist
   deshalb aus), signierte SHA256SUMS, `bz release`, Rückfall per Boot-Counting (entscheiden:
   zählt ein Start mit fehlgeschlagener Unit als gescheitert?); Smoke: Update A→B mit TPM,
   kaputtes Update → A, zweites Update.
