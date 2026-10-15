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
| `tools/bz/src/` | Build-Werkzeug: `source` `recipe` `packages` `install` `kernel` `layout` `tree` `usr` `initrd` `microcode` `secureboot` `image` `installer` `debuginfo` (MiniDebugInfo je Paket) `stamp` (Fingerabdrücke der Schritte; `build.rs` hasht den Code, der Ergebnisse bestimmt) `boottimes` `hwcheck` `outdated` `paths` `util` (`run`, `say!`/`capture`); `vm/`: `machine` `console` (`Session`) `report` `checks` `run` `debug`, `smoke/` (`mod.rs` Ablauf; Szenarien in `trust` `access` `updates` `integrity`) |
| `recipes/*.toml` | ein Rezept pro Paket: Quelle (gepinnt + signiert, `latest` für `bz outdated`), `deps`, `stage`, `[build] script`, zuletzt die Metadaten `[sbom]` (Lizenz, CPE, ADR 0022) und `[files]` (was ins Image kommt); ohne `stage` nur Quelle (linux, linux-firmware, intel-microcode) |
| `keys/` | Maintainer-Schlüssel + `README.md`; `microsoft/` (gepinnt); `secureboot/` öffentliche Produktionsschlüssel + Anleitung für den Besitzer |
| `images/` | `base.toml` Pakete für `/usr` · `initrd.toml` Funktionen und Module der initrd (ADR 0016, Auswahl in `bz`: `select.rs`) · `base.preset`/`user.preset` aktivierte System-/Benutzer-Units · `running-units.txt` was im Smoke-Test laufen muss (ADR 0013) · `security.toml` gemessene Härtung der Dienste |
| `system/usr/`, `system/initrd/` | eigene Dateien (Units, Boot-Report, Recovery-Key …); in der initrd bash, aber **keine coreutils** |
| `kernel/config/` | Kernel-Fragmente nach Funktion, jede Option begründet, auf `alldefconfig` + Upstream-`hardening.config` (ADR 0015); `kernel/beize.config` = aufgelöste Konfiguration, Änderungen im Diff prüfen und mitcommitten |
| `ci/buildroot/` | Buildroot (ADR 0021): `packages.txt` (einzige Liste der Werkzeuge) → `lock.sh` → `packages.lock` (Versionen + SHA-256, Snapshot-Datum in `snapshot`); `create.sh`, `hash.sh` |
| `ci/system.sh` | kompletter CI-Ablauf (läuft im Buildroot-Image) |

## 6. Bauen und Testen (in `~/beize`)

```sh
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
./bz exec cargo deny check   # Rust-Abhängigkeiten: Advisories, Lizenzen, Herkunft (deny.toml)
./bz exec ci/fuzz.sh [s]     # Fuzzing (fuzz/), je Ziel s Sekunden; Funde in build/fuzz/artifacts
./bz build           # kernel → Pakete → usr → initrd → image; jeder Schritt nur, wenn sich seine Eingaben ändern
./bz smoke --quick   # Installation + 2 Starts mit allen Prüfungen (~30 s): die schnelle Schleife
./bz smoke           # DER Abnahmetest (~2 min, Szenarien parallel), vor jedem Commit grün
./bz debug 'journalctl -b -p warning' …   # Befehle auf der Root-Shell einer VM, Ausgabe auf stdout
#   --machine <name> (von smoke liegengelassen) · --tpm <dir> · --recovery-key … · '@wait N'
./bz run             # interaktiv (Ctrl-A X); VM-Disk bleibt bis zum nächsten Image
./bz boot-times ~/hw-logs/journal [--boot -1]   # Phasen, Kernel-Lücken, langsamste Units, Kette
./bz hw-check ~/hw-logs/journal   # welches PCI-Gerät des Rechners im Image einen Treiber hat
./bz outdated        # neuere Upstream-Releases aller Quellen
./bz cve             # bekannte Lücken im Image: NVD (rot ab CVSS 7) und RustSec für fremde
                     #   Crates (`[sbom] cargo-lock`, rot bei jeder); Bewertung in [sbom.cve-ignore] (~6 min)
./bz verify · ./bz package <name> · ./bz owner /usr/bin/bash · ./bz -v …  (alles ausgeben)
# --locked (CI): kernel/beize.config muss aktuell sein, statt neu geschrieben zu werden
# nur der Besitzer, im Terminal, mit 1Password: ./bz secureboot new-keys|new-update-key|new-module-key ·
#   ./bz sign · ./bz release (signiertes Update nach build/out/release/<ver>) · ./bz install
```

- `./bz` läuft im Buildroot (ADR 0021, bwrap; legt ihn bei Änderung an `ci/buildroot/` neu
  an, ~4 min, Log `build/buildroot/create.log`). Nur `bz install` läuft auf dem Rechner selbst.
  `cargo test` usw. laufen auch außerhalb; `bz` selbst baut nur im Buildroot.
- Ausgabe ist knapp: Befehle zeigen nur bei Fehler ihre letzten 40 Zeilen. Logs:
  `build/work/logs/<paket>.log`, `linux.log`, Konsole jedes Starts in
  `build/work/logs/boot/NN-<maschine>.log` (gehört immer zum letzten smoke/debug/run),
  daneben das Journal jedes Starts mit Boot-Report als `.journal.json` (über eine leere virtio-Disk;
  `bz boot-times <datei>`). Smoke zeigt beim Start mit Zeitbudget die langsamsten Units.
  Die Konsolengröße der VM steht in der Kommandozeile (`systemd.tty.*`, sonst fragt PID 1 2× 333 ms).
- Zeiten: nichts geändert 0,1 s; Datei in `system/usr` 1,6 s; Änderung nur an Test-/Berichtscode
  von `bz` (`vm/`, `cve` …) baut nichts neu; Rezeptänderung an systemd: 24 s für systemd, dazu was davon abhängt; Bau von null
  9,6 min (2026-10-14, davon Toolchain 6 min, Kernel danach parallel zu den Paketen),
  Plattenspitze 7 GiB. Alles Unsignierte ist auf jedem Rechner gleich (`ci/digest.sh`); signiert
  wird je Schlüsselsatz beim Zusammensetzen, Signaturen bleiben bis zum nächsten Kernel-Bau
  (ML-DSA signiert mit Zufall; ADR 0015) → gleicher Stand, gleiches `disk.img`. Die CI baut
  wöchentlich von null.
- Was `bz smoke` prüft, steht in `tools/bz/src/vm/smoke/` (Modul-Doku in `mod.rs`) und
  `vm/checks.rs`. Ein gescheitertes Szenario lässt seine Maschine liegen
  (`bz debug --machine smoke-db` usw.).
- Start in der VM: 3,8 s Gast-Uptime, erster Start ~6 s, Smoke lokal ~105 s. Der zweite Start
  hat ein Zeitbudget je Phase (`BOOT_BUDGET` in `vm/checks.rs`, ~1,5× CI-Messwert); bei
  Änderung neu messen und den Kommentar aktualisieren.
- CI (`.github/workflows/ci.yml`): Job `buildroot` legt das Buildroot-Image in GHCR an, wenn
  es fehlt (`beize-buildroot:<hash>`; `mirror.yml` liefert nur das Ubuntu dafür); darin
  fmt/clippy/test bei jedem Push; auf `main`, manuell und wöchentlich (dann von null)
  `ci/system.sh` privilegiert mit KVM. Toolchain, Sysroot, Stamps, Kernel-Objektbaum, Microcode
  und die Test-Schlüssel im Actions-Cache; `build/out` nicht, daher laufen die Schritte ab Kernel
  dort immer. Der Runner ist klein (Smoke-Szenarien nacheinander, ~14 min). Das Repo ist
  öffentlich (Besitzer, 2026-10-14) – Standard-Runner öffentlicher Repos haben 4 vCPU/16 GB;
  `ci/system.sh` gibt die Größe aus (`build host:`), bis 2026-10-13 waren es 2 CPUs.

## 7. ADRs (`docs/adr/`)

0001 Kernel aus Upstream · 0002 glibc + systemd · 0003 unveränderliches signiertes Image ·
0004 WSL2 · 0005 Cross-Toolchain, Rezepte, signierte Quellen · 0006 Secure Boot, Test-
vs. Produktionsschlüssel · 0007 nur `/usr`, A/B, erster Start · 0008 dm-verity · 0009 Root
mit LUKS2 + TPM · 0010 Plattform-Eigentümer, Profile, Neubinden, echte Hardware · 0011
Netzwerk · 0012 Zeit (NTS) · 0013 Was läuft (Units im Image) · 0014 Toolchain x86-64-v3 + Härtung · 0015 Kernel: Konfiguration, Module, Microcode, Firmware · 0016 initrd aus Funktionen, Notfall-Shell, hwdb · 0017 Tastatur (XKB, kbd-Keymaps, Keymap der initrd in EFI-Variable) · 0018 Rechte (run0 + polkit, kein setuid, Wiederherstellung) · 0019 Konten (Beizes Kontodienst, `/etc/userdb`, Rechte durch Beweis; homed verworfen) · 0020 Updates (beize-updated + systemd-sysupdate, ML-DSA-signierte Liste, 4-GiB-Slots) · 0021 Buildroot (gepinnt, abgeschottet, lokal = CI) · 0022 SBOM und Schwachstellen (NVD, RustSec) · 0023 Absturzberichte (MiniDebugInfo, elfutils). Neue Architekturentscheidung = neues ADR; spätere Änderungen
in die Endfassung einarbeiten, keine Nachträge.

## 8. Regeln für Rezepte und Code

- Nur `/usr` ausliefern: `bz` bricht ab bei Dateien außerhalb; Altlasten im Rezept
  löschen (mit Kommentar) oder nach `/usr/share/factory` verlegen.
- **`[files]` (ADR 0005, `files.rs`):** jede Datei eines Pakets, die nicht ihrer Art nach
  nur zum Bauen ist, steht in `image` (Programme/Bibliotheken in `usr/bin`, `usr/lib`
  einzeln, tiefer auch `dir/**`) oder in `build` mit `why` (nimmt auch aus Image-Mustern
  heraus). Installiert ein Paket Neues, scheitert der Bau, bis es eingeordnet ist.
- pkg-config je Maschine (ADR 0005): Zielpakete fragen `x86_64-beize-linux-gnu-pkg-config`
  (= `pkg-config`/`$PKG_CONFIG` in ihrem PATH; pkgconf-Personality, FDO-Regel: Variablen
  bleiben Zielpfade). Werkzeuge für die Baumaschine über Meson `--native-file "$MESON_NATIVE"`
  (jedes Meson-Rezept übergibt es). `bz` bricht ab, wenn ein Sysroot-Pfad in `/usr` landet.
- `meson -Dauto_features=disabled`: jedes benötigte Feature ausdrücklich einschalten.
- Abhängigkeitsschleifen vermeiden (libdevmapper ohne udev-Sync: systemd → cryptsetup → lvm2).
- **Strippen ist Sache von `bz`** (ADR 0023): Nach jedem Paketbau bekommt jede ELF-Datei
  MiniDebugInfo und wird gestrippt; ein Rezept strippt nie selbst (kein `install -s`,
  `strip = true`), sonst scheitert der Bau. `.comment` bleibt (ELF-Prüfung).
- **Lücke in einem Crate eines fremden Rust-Programms** (`bz cve`): behobenes Patch-Release per
  `[cargo.update]` im Rezept (Version, `why` mit RUSTSEC-IDs; ADR 0022), nie den Code patchen;
  `bz` meldet, wenn der Eintrag überflüssig wird. Nicht gelinkte Crates (nur im Cargo.lock) nicht.
- **Eigener Rust-Code im Image:** Crate unter `os/<name>`, Rezept mit `repo = [...]`
  nach Vorbild `recipes/beize-tpm.toml`. Crates mit Build-Skript meiden (laufen mit
  dem Host-cc). Krypto nie selbst schreiben.
- Rezepte **ohne** `repo` behalten ihren Fingerprint (Test) – sonst baut alles neu.
- **Ein Paket baut gegen ein eigenes Sysroot nur aus seinen (transitiven) `deps`**
  (ADR 0005). Den Compiler nur über `$CC`/`$CXX` aufrufen (Makefiles, die ihn selbst
  zusammensetzen: `CC="$CC"` übergeben); fehlt etwas, gehört es in `deps`.
- Pakete bauen parallel (ADR 0005): ein Jobserver für alle (512 MiB je Job, 2 GiB
  Reserve), Start nur mit Speicherprüfung, je Paket eine cgroup. Im Rezept **nur
  Bauschritte** mit `MAKEFLAGS="$JOBSERVER"` (nie `-j`), Installieren seriell.
  Parallele VMs nach CPUs/RAM (je 4 CPUs und 3 GiB reserviert, Gast 2 GiB).
- Werkzeuge, die zur systemd-Version im Image passen müssen (ukify, systemd-measure),
  kommen aus unserem systemd-Build (ADR 0009).
- Host-Werkzeuge, die `bz` oder ein Bau aufruft, gehören in `ci/buildroot/packages.txt`
  (danach `ci/buildroot/lock.sh`; neuer Buildroot = alles baut neu). Im Gast gibt es kein
  `sed`/`awk`/`grep` (Skripte in reinem Bash).
- Autotools-Abfragen beim Host (`AC_CHECK_PROG`) im Rezept beantworten (`ac_cv_…=yes`).
- Skripte unter `system/` müssen ausführbar sein (Test prüft das).
- **Units aktivieren nur über `images/base.preset`** (Benutzer-Units: `images/user.preset`;
  mit Grund; `bz` legt die Links in `/usr`), nie über Presets oder `.wants`-Links in
  `system/`. Ändert sich, was läuft, `images/running-units.txt` mitpflegen (ADR 0013).
- **Kein Pfad der Baumaschine im Image:** die Compiler bilden das Repo auf `/beize` ab;
  Rezepte mit eigenen CFLAGS hängen `$PREFIX_MAP` an. `bz` bricht ab, wenn eine Datei in
  `/usr` den Repo-Pfad nennt. Rust-Programme fremder Projekte (ntpd-rs) schreiben beim Bau
  ihre Crates nach `usr/lib/beize-build/crates/` (SBOM).
- **Härtung als Ratsche:** `images/security.toml` hält `systemd-analyze security` jedes
  eigenen Dienstes und jedes erreichbaren Daemons fest; `bz` bricht bei jeder Abweichung
  ab (schlechter = zurücknehmen, besser = Wert eintragen). Neuer `beize-*`-Dienst = Eintrag.
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
  suchen, Homepage prüfen) und `[sbom]` mit `license` (SPDX, Fedoras Angabe) und `cpe`
  (Liste, gegen NVDs CPE-Wörterbuch geprüft; leer nur mit Begründung); Test verlangt beides.
  Die SBOM des Images: `build/out/sbom.spdx.json`.
- **Eigener Rust-Code:** `unsafe` nur mit `// SAFETY:`, kein `unwrap`/`expect` außerhalb von
  Tests (Lints). Was ein unprivilegierter Aufrufer schickt, läuft durch einen Fuzz-Einstieg
  (`beize_varlink::fuzz_request`, `beize_update::list::fuzz_list`, Ziele in `fuzz/`); neue
  Eingaben dorthin. Ein behobener Fund kommt nach `fuzz/regressions/<ziel>/` (cargo test).
- Ausgaben in `bz` über `say!` (nicht `eprintln!`), damit parallele Szenarien sie
  gesammelt zeigen; Prüfungen im Smoke-Test müssen belegen, dass ihre Bedingung
  hergestellt wurde (Gegenprobe: Eigenschaft absichtlich brechen → rot).

## 9. Bekannte Fallstricke

- WSL-VM nach OOM instabil → `wsl --shutdown`. JetBrains-Backend: `build/` ausschließen.
- WSL beendet die Distro ohne angehängte `wsl.exe`-Sitzung → lange Jobs im Vordergrund
  oder mit gehaltener Sitzung; Logs nach `~`, nicht `/tmp`.
- `qemu -serial stdio` liest stdin – in Skripten `< /dev/null`.
- **CI rot, lokal grün?** Werkzeuge sind gleich (Buildroot); Unterschiede kommen vom Runner:
  2 CPUs, 7,8 GiB, root, kein Service-Manager (keine cgroups, Speicher nur per Startprüfung),
  Bau von null (Cache leer oder wöchentlich). Lokal von null: `build/` außer `cache/` leeren.
- `actions/cache` braucht `zstd` im Container (im Buildroot enthalten), sonst verfehlen sich
  Sichern (zstd) und Wiederherstellen (gzip).
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
- Bit-gleiche Bauten: alles, was eine Zeit schreibt, bekommt `SOURCE_DATE_EPOCH`
  (systemd-sbsign, mkfs.vfat, mtools) oder normalisierte mtimes (cpio). `sbvarsign` kann das
  nicht: die `.auth` der Test-Schlüssel werden deshalb nur einmal je Schlüsselsatz signiert.
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

## 11. STAND UND NÄCHSTE AUFGABE (2026-10-15)

**Beize läuft auf dem Rechner des Besitzers**, installiert ist 0.3 (Toolchain-Kernel 7.2.10;
Disks, Messwerte, Update vom Stick: `docs/hardware.md`; neu installieren:
`keys/secureboot/README.md`). Wiederherstellung: nach F8 Leertaste halten, „(Recovery)“,
Recovery-Key, `s`.

**PHASE 7 – Konsolidierung (seit 2026-10-12).** Ausgangswerte: WSL `~/p7/BASELINE.md`; jede
Änderung mit Vorher/Nachher, je Thema ein Commit. Erledigt (Einzelheiten in Commits und ADRs):
- 2026-10-12/13: Bau von null repariert, `/usr` 80 → 41 MiB, zstd, verity-Tor 91 → 1 s, paralleles
  Bauen (#3), pkg-config je Maschine, Buildroot (ADR 0021), `[files]`, cargo-deny, SBOM, `bz cve`
  (ADR 0022), Härtungs-Ratsche, Lints, Fuzzing, MPL-2.0.
- 2026-10-14, Prüfung gegen etablierte Praxis (Funde als Issues #29–#36): Kernel mit Beizes
  Toolchain (#29) · Hardlinks bleiben (#30) · gcc-pass1 nur C (#31) · Stempel nur am bauenden
  Code (#27) · **dbus-broker** statt dbus-daemon, `DynamicUser=` geht wieder (#32, #28) ·
  ntpd-rs-Crates in der SBOM (#24) · Zeitbudget vom 2-CPU-Runner (#9) · `vm/smoke/` nach Themen
  (#7) · linux 7.2.10, curl 8.23, mpc 1.4.1 · ccache gemessen, lohnt nicht (#4). Bau von null 9,6 min.
- 2026-10-14, #5 (Variante A, vom Besitzer gewählt): Module je Schlüsselsatz signiert, getrennt vom
  Bau (ADR 0015); Produktions-Modulschlüssel vom Besitzer angelegt (`module.crt`). #33: MiniDebugInfo
  je Paket, systemd mit elfutils, Stacktraces in `coredumpctl` (ADR 0023; `-g` gemessen, vorerst
  nicht). #24: `bz cve` prüft fremde Crates mit cargo-audit (Buildroot: cargo-audit, bzip2,
  Wiederholungen gegen Ausfälle von snapshot.ubuntu.com).

**PLAN BIS M3 (mit dem Besitzer abgestimmt 2026-10-14)**
1. ✓ 2026-10-15: CI grün (#37 und 0.3 gepusht), Runner 4 vCPU (#2 geschlossen).
2. #5 (15.10.): behoben, was zwei Bauten unterschied – Beizes Git-Commit in iputils und ntpd-rs
   (`GIT_CEILING_DIRECTORIES`), der Bau-Pfad in gcc-pass1s libgcc, die umask (`./bz` setzt 0022;
   775 statt 755). `ci/digest.sh` sah Rechte nicht: jetzt Typ, Rechte, Symlink und Inhalt je Eintrag,
   dazu initrd-Baum und /usr-Abbild; die Listen in `build/out/digest/`, die CI lädt sie hoch
   („digest-lists“). CI 38773182214 (`ae15fc3`) war grün, /usr und initrd wichen noch ab (umask
   oder anderes). **Nächster Chat:** CI-Lauf 38789354237 (`b3289157`, Bau von null) prüfen: Artefakt „digest-lists“ laden
   (`gh run download <id> -n digest-lists`) und mit lokal `./bz build && ci/digest.sh` Datei für
   Datei vergleichen (`diff`); gleich → #5 schließen. Danach `~/repro` löschen (Wegwerf-Klone).
   bz bricht jetzt ab, wenn `configure` Optionen ignoriert (fand curl `--disable-smb`, e2fsprogs
   `--disable-static`, util-linux `--enable-findmnt`).
   **Grundwerkzeuge (15.10., vom Besitzer gewollt):** grep (PCRE2), sed, gawk, findutils, diffutils,
   gzip, procps-ng, less, ncurses (Terminals wie Fedoras ncurses-base), iproute2 (ohne tc), xz- und
   zstd-Programme; `tar` ist bsdtar aus libarchive (wie macOS) – GNU tar hat seit 1.35 (2023) kein
   Release, Fedora flickt 3 CVEs und den acl-2.4-Konflikt. Smoke prüft jedes Werkzeug mit Antwort;
   die Test-Shell schaltet den Pager ab (sonst hängt journalctl in less). Offen, Besitzer entscheidet:
   findmnt/flock (util-linux von `--disable-all-programs` auf eine ausdrückliche Liste umstellen?),
   Editor (vi oder nano), file, patch, strings.
3. **Hardware-Test 0.3 ausgewertet (15.10., #8):** 0 fehlgeschlagene Units, Produktions-
   Modulschlüssel geladen, dbus-broker läuft; ohne F8: Firmware 15,5 s, ab Loader 13,6 s
   (`docs/hardware.md`). Behoben (`9aea966`…`8f26050`, Push erst nach grünem CI 38789354237): `bz boot-times`/`hw-check`
   fanden Journale außerhalb des Repos nicht (seit ADR 0021); `journalctl --grep` ohne PCRE2;
   **Bootloader-Fix ADR 0024** (loader.conf in `/usr`, `beize-boot-update` gleicht nach
   erfolgreichem Start ab, `console-mode max`, Bericht `display=`/`entry=`). OVMF bildet das
   Herunterschalten nicht nach (gemessen) – Beleg nur auf Hardware: 0.3 Menü-Start `128x48`
   (1024×768), ohne Menü `160x45` (2560×1440). Die Absturz-Probe in `~/hw-test-0.3.md` war ein
   Wettlauf (`kill` vor dem Start von sleep → nur `_start`); Smoke prüft es richtig. Bekannt,
   unverändert: growfs `crypt_resize` EPERM, i2c ITE8853 -16 (#22). **Leerlauf fehlt (#6).**
   **Besitzer:** Hardware-Test 0.4 nach WSL `~/hw-test-0.4.md` (Commit „Beize 0.4“): erster
   Start, Menü-Start, Grundwerkzeuge, Leerlauf ≥ 3 h. **Nächster Chat wertet aus:** im ersten
   Start von 0.4 `Updated /boot/loader/loader.conf`, Menü-Start `Console: switching … 160x45`,
   idle-1 gegen idle-2 (MemoryCurrent und CPUUsageNSec je Unit, `free -m`: nichts wächst) →
   #6 schließen; Phase 7 und M2 fertig, §11 für M3 aufräumen, Besitzer darf den Chat leeren.
   Offene Frage des Besitzers: ~700 MB RAM „gut oder viel?“ – Quelle unklar (cgtop/meminfo
   zählen den Seitencache mit); mit `idle-*.txt` beantworten.
4. M3 planen (eigener Chat). Mesa braucht sehr wahrscheinlich LLVM (llvmpipe, mesa-clc); grob geschätzt lokal
   +15 min Bau von null, CI auf 4 vCPU +1–1,5 h – messen, bevor geplant wird. Mit dem ersten
   C++-Programm den Demangler von elfutils einschalten (ADR 0023).
Nebenbei: #7 Rest (`secureboot.rs`, `source.rs`, `select.rs`). Zurückgestellt: #22 growfs-Bericht,
#4 vmspawn. #10 seit 14 Läufen nicht wieder – beobachten. **Label `m3-plan` (#14, #17, #34–#36) nur
mit Zustimmung des Besitzers** (zram/oomd, nftables, fwupd, BTF, Treiber).
Runner: root, kein systemd, 4 vCPU seit das Repo öffentlich ist (Bau von null zuvor ~2 h auf 2).
Der `gh`-Token darf
keine Workflows starten. Der alte `git stash` (13.10.) ist der Zwischenstand von #3, ganz in
`941f5c6` aufgegangen; Kopie `~/stash.patch` – Besitzer löscht ihn (`git stash drop`).

**Fahrplan bis M2 (vom Besitzer freigegeben):** je Phase ein Chat, ADR vorab abstimmen, nichts
abschalten „weil unnötig“, keine fremden Patches; bei Fehlern zuerst fragen, welche eigene
Entscheidung uns auf einen ungetesteten Pfad gebracht hat. Phasen 1–6 ✓ (Messen, Toolchain, Kernel,
Startpfad, Grundfunktionen, A/B-Updates; ADR 0013–0020). **7. Konsolidierung vor M3** (läuft):
Funktionen bleiben, geändert wird nur nachweislich Besseres; nicht feinschleifen, was M3 ersetzt
(Konsole, Setup-Oberfläche).
