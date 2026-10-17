# Beize – Übergabe für Claude Code

Einstieg für jede neue Session; ersetzt den Chatverlauf. Stand: siehe §11. Was im Code,
in den ADRs, im README oder in `docs/` steht, steht hier nicht noch einmal.

## 1. Was Beize ist

Ein eigenes Betriebssystem auf dem Linux-Kernel: **so konsequent wie macOS, so
frei wie Windows, technisch so offen wie Linux.** Kernel, Treiber und
Basisbibliotheken kommen aus Upstream (selbst gebaut), alles darüber –
Compositor, Shell, Einstellungen, Dienste – wird eigener Code (Rust).
**Lies vor größeren Entscheidungen `docs/VISION.md`.** Meilensteine: M1 Bootet in
QEMU ✓ · M2 Echte Hardware ✓ · **M3 Compositor (als Nächstes)** · M4 Alltagstauglich ·
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
  Polling-Schleifen – Actions-Minuten sind knapp. Gepusht wird erst, wenn der vorige Lauf
  (schnelle Stufe) grün ist; den nächtlichen vollen Lauf prüft Claude zu Beginn der Arbeit.
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
wird ausgeliefert; `/etc` entsteht beim 1. Start aus Links nach `/usr` (Beizes Vorgaben,
`tmpfiles.d/beize.conf`), systemds `/usr/share/factory/etc` und von
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
| `images/` | je Variante ein Ordner (ADR 0025, `bz`: `variant.rs`): `base/`, `desktop/` (erweitert `base`, fügt nur hinzu) mit `variant.toml` Name und Pakete für `/usr` · `system.preset`/`user.preset` aktivierte System-/Benutzer-Units · `running-units.txt` was im Smoke-Test laufen muss (ADR 0013) · `security.toml` gemessene Härtung der Dienste; eine Datei ohne Ergänzung fehlt beim Desktop. Gemeinsam: `initrd.toml` Funktionen und Module der initrd (ADR 0016, Auswahl in `bz`: `select/`) |
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
#   --variant base|desktop (Vorgabe desktop, ADR 0025) gilt für alle Befehle; je Variante
#   build/out/<variante>/ und build/work/variant-<variante>/ (usr-Baum, VMs); Kernel, initrd geteilt
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
#   ./bz sign · ./bz release (signiertes Update nach build/out/<variante>/release/<ver>) · ./bz install
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
  fmt/clippy/test bei jedem Push; auf `main` `ci/system.sh` privilegiert mit KVM, in zwei
  Stufen (17.10., Besitzer): **jeder Push** `SMOKE=quick` (beide Varianten, Budget ~5 min),
  **nachts** (nur für einen Commit ohne vollen Lauf), **manuell** und **wöchentlich** (dann
  von null, mit `bz verify`, CVE, Fuzzing) `SMOKE=full` auf dem Desktop. Ein Push, der nur
  Doku ändert (`*.md`, `docs/`), baut nichts. Neue Szenarien kommen in die volle Stufe,
  nie in jeden Push. Ein Release nur von einem Commit mit grünem vollem Lauf. Toolchain, Sysroot, Stamps, Kernel-Objektbaum, Microcode
  und die Test-Schlüssel im Actions-Cache; `build/out` nicht, daher laufen die Schritte ab Kernel
  dort immer. Runner: 4 vCPU/16 GB (öffentliches Repo), Smoke-Szenarien nacheinander;
  `ci/system.sh` gibt die Größe aus (`build host:`).

## 7. ADRs (`docs/adr/`)

0001 Kernel aus Upstream · 0002 glibc + systemd · 0003 unveränderliches signiertes Image ·
0004 WSL2 · 0005 Cross-Toolchain, Rezepte, signierte Quellen · 0006 Secure Boot, Test-
vs. Produktionsschlüssel · 0007 nur `/usr`, A/B, erster Start · 0008 dm-verity · 0009 Root
mit LUKS2 + TPM · 0010 Plattform-Eigentümer, Profile, Neubinden, echte Hardware · 0011
Netzwerk · 0012 Zeit (NTS) · 0013 Was läuft (Units im Image) · 0014 Toolchain x86-64-v3 + Härtung · 0015 Kernel: Konfiguration, Module, Microcode, Firmware · 0016 initrd aus Funktionen, Notfall-Shell, hwdb · 0017 Tastatur (XKB, kbd-Keymaps, Keymap der initrd in EFI-Variable) · 0018 Rechte (run0 + polkit, kein setuid, Wiederherstellung) · 0019 Konten (Beizes Kontodienst, `/etc/userdb`, Rechte durch Beweis; homed verworfen) · 0020 Updates (beize-updated + systemd-sysupdate, ML-DSA-signierte Liste, 4-GiB-Slots) · 0021 Buildroot (gepinnt, abgeschottet, lokal = CI) · 0022 SBOM und Schwachstellen (NVD, RustSec) · 0023 Absturzberichte (MiniDebugInfo, elfutils) · 0024 Bootloader-Konfiguration mit jeder Version · 0025 Varianten `base`/`desktop` (Vorgabe `desktop`, Start ohne Oberfläche) · 0026 TPM-Schlüssel für „(Basis)“ (mit M4). Neue Architekturentscheidung = neues ADR; spätere Änderungen
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
  (danach `ci/buildroot/lock.sh`; neuer Buildroot = alles baut neu). In der initrd gibt es
  keine coreutils, kein `sed`/`awk`/`grep`: Skripte dort in reinem Bash.
- Autotools-Abfragen beim Host (`AC_CHECK_PROG`) im Rezept beantworten (`ac_cv_…=yes`).
- Skripte unter `system/` müssen ausführbar sein (Test prüft das).
- **Units aktivieren nur über `images/<variante>/system.preset`** (Benutzer-Units:
  `user.preset`; mit Grund; `bz` legt die Links in `/usr`), nie über Presets oder
  `.wants`-Links in `system/`. Ändert sich, was läuft, `running-units.txt` derselben
  Variante mitpflegen (ADR 0013). Was nur der Desktop braucht, gehört nach `desktop/`.
- **Kein Pfad der Baumaschine im Image:** die Compiler bilden das Repo auf `/beize` ab;
  Rezepte mit eigenen CFLAGS hängen `$PREFIX_MAP` an. `bz` bricht ab, wenn eine Datei in
  `/usr` den Repo-Pfad nennt. Rust-Programme fremder Projekte (ntpd-rs) schreiben beim Bau
  ihre Crates nach `usr/lib/beize-build/crates/` (SBOM).
- **Härtung als Ratsche:** `images/<variante>/security.toml` hält `systemd-analyze security` jedes
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
  Die SBOM des Images: `build/out/<variante>/sbom.spdx.json`.
- **Eigener Rust-Code:** `unsafe` nur mit `// SAFETY:`, kein `unwrap`/`expect` außerhalb von
  Tests (Lints). Was ein unprivilegierter Aufrufer oder die Firmware schickt, läuft durch einen
  Fuzz-Einstieg (`beize_varlink::fuzz_request`, `beize_update::list::fuzz_list`, beize-tpm
  `efi::fuzz_firmware`, Ziele in `fuzz/`); neue Eingaben dorthin. Ein behobener Fund kommt nach `fuzz/regressions/<ziel>/` (cargo test).
- Ausgaben in `bz` über `say!` (nicht `eprintln!`), damit parallele Szenarien sie
  gesammelt zeigen; Prüfungen im Smoke-Test müssen belegen, dass ihre Bedingung
  hergestellt wurde (Gegenprobe: Eigenschaft absichtlich brechen → rot).

## 9. Bekannte Fallstricke

- WSL-VM nach OOM instabil → `wsl --shutdown`. JetBrains-Backend: `build/` ausschließen.
- WSL beendet die Distro ohne angehängte `wsl.exe`-Sitzung → lange Jobs im Vordergrund
  oder mit gehaltener Sitzung; Logs nach `~`, nicht `/tmp`.
- `qemu -serial stdio` liest stdin – in Skripten `< /dev/null`.
- **CI rot, lokal grün?** Werkzeuge sind gleich (Buildroot); Unterschiede kommen vom Runner:
  4 vCPU, 16 GiB, root, kein Service-Manager (keine cgroups, Speicher nur per Startprüfung),
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

**GitHub Issues** (`gh issue list`; Labels `m3-plan`, `upstream`, `hardware`). Neue Punkte dort
anlegen, nicht hier; ein Commit, der einen Punkt erledigt, nennt `Fixes #N`. Upstream-Entwürfe
liegen in WSL `~/upstream/` (sendet der Besitzer, #13).

## 11. STAND UND NÄCHSTE AUFGABE (2026-10-17)

**M2 ist abgeschlossen** (Beize 0.4 auf der Hardware des Besitzers geprüft; Messwerte und Leerlauf:
`docs/hardware.md`). Installiert ist 0.5 (Desktop-Strom), am 17.10. vom Besitzer geprüft:
Update von 0.4, `/etc`-Links, „(Basis)“ – alles in Ordnung. Neu installieren: `keys/secureboot/README.md`, Update vom
Stick: `docs/hardware.md`. Wiederherstellung: nach F8 Leertaste halten, „(Recovery)“, Recovery-Key,
`s`. Phase 7 (Konsolidierung, Review #7) steht in Commits, ADR 0021–0024 und geschlossenen Issues.

**M3 – Compositor. Grundsatz (vom Besitzer am 16.10. entschieden): modular.** Beize besteht aus
einer **Basis** (alles, was M2 ist: Kernel, systemd, Netz, Zeit, Updates, Konten, TPM/LUKS,
Wiederherstellung – ein vollständiges System ohne Oberfläche) und dem **Desktop** darüber
(Grafik, Compositor, Shell, Schriften, Ton). Ziel: jederzeit eine Variante ohne Oberfläche
bauen können, später „Beize Server“. Vorbild: Windows Server Core (dasselbe System ohne
Shell), Fedora-Editionen aus denselben Paketen. Kein Fork, sondern Varianten eines Baus.
Keine Paketverwaltung für Nutzer (VISION: Apps als Bundles, Modell vor M4) – die Varianten
entstehen beim Bauen.

**ADR 0025 (Varianten) ist umgesetzt** (17.10., alle Teile gepusht):
- Varianten in `bz` (`variant.rs`, `--variant`, Vorgabe `desktop`). Der Baum von `base` ist
  bitgleich zu vorher bis auf os-release.
- Update-Strom je Variante (`beize-<v>_…`). Der Updater nimmt nur seine Variante, das prüft
  der Smoke-Test. Der Übergang von 0.4 (`legacy/` im Release 0.5) ist erledigt und entfernt.
- „(Basis)“ als UKI-Profil 2, vorerst mit dem Recovery-Key (Smoke-Szenario
  `WithoutAdditions`). Ein eigener TPM-Schlüssel dafür (ADR 0026) ist angenommen, kommt aber
  erst mit M4 (#38).
- Werkzeuge (Besitzer: „nur Consumer, wie macOS“): findmnt und flock aus util-linux (jetzt
  ohne `--disable-all-programs`; `[files] build` nennt jedes weggelassene Programm mit
  Grund), `file` neu (eigene Sandbox mit seccomp). patch und strings bleiben draußen.
  `/etc/profile` und `/etc/ld.so.conf` sind Links nach `/usr` (ADR 0007).

**Smoke-Test, gemessen 17.10.:** Release für beide Update-Szenarien einmal gebaut;
Installation läuft allein (ihre Startmessung). Zwei VMs auf 4 CPUs brachten nichts (579
statt 585 s, ein Timeout): Starts rechnen. Firmware-Zeit je Start gemessen (17.10.,
#39): Der größte Posten ist die TPM-Emulation (QEMU/swtpm, ~1 s fest, bis 1 s zufällig),
nicht Beize; Ursache dort noch offen, später, nach Absprache mit dem Besitzer.

**`OS_VERSION` ist 0.5 (17.10.)** und läuft auf dem PC des Besitzers. „(Basis)“ zeigt dort
`VARIANT_ID=desktop` – richtig, denn es ist dasselbe `/usr` mit anderem systemd-Ziel (ADR 0025);
für den Besitzer steht das jetzt in `keys/secureboot/README.md`. **Zu Beginn des nächsten
Chats:** den nächtlichen vollen CI-Lauf prüfen (`gh run list --event schedule`), dann den
Grafik-Plan (Aufgabe 1 unten) als ADR-Entwurf mit dem Besitzer erarbeiten – noch nichts bauen.

**Nächste Aufgaben, in dieser Reihenfolge:**
1. M3-Grafik **planen, bevor gebaut wird**, mit dem Besitzer (ADR): offen sind laut
   `docs/VISION.md` Grafik-API des Compositors, UI-Toolkit, Systemschrift und Icon-Stil,
   dazu NVIDIA (#17: Treiber, Firmware). Mesa braucht sehr wahrscheinlich LLVM (llvmpipe,
   mesa-clc): grob lokal +15 min Bau von null, CI +1–1,5 h, also vorher messen. Mit dem
   ersten C++-Programm den Demangler von elfutils einschalten (ADR 0023). Desktop-Pakete
   kommen nach `images/desktop/` (ADR 0025).

   **Was der Besitzer am 17.10. dazu gesagt hat** (Richtung für den ADR, noch nicht
   entschieden; macOS als Maßstab, „alles aus einem Guss“):
   - **Compositor:** ein eigener Beize-Compositor, der zugleich die Shell ist, auf
     **Smithay** (Rust; Smithay übernimmt Wayland-Protokoll, DRM/KMS, Puffer und Eingabe,
     Beize alles Sichtbare). Der Besitzer will das so.
   - **Rendern mit Vulkan** (Besitzer: „Vulkan ist geil“).
   - **NVIDIA:** Der Besitzer zieht offizielle Treiber vor, aber **wichtiger sind der
     neueste Kernel und dass alles ohne Probleme läuft**. Für den ADR heißt das: NVIDIAs
     offene Kernel-Module plus geschlossene Bibliotheken nur, wenn sie den jeweils
     neuesten stabilen Kernel verlässlich unterstützen. Sonst Nouveau mit NVK (Mesa). Das
     mit Daten prüfen: Wie lange brauchte NVIDIA in der Vergangenheit für neue Kernel,
     und wie gut ist NVK auf der RTX 3070 Ti?
   - **UI-Toolkit:** offen. Der Besitzer hätte am liebsten ein **eigenes**, wenn es weniger
     Versuch und Irrtum braucht als der Rest des OS. Pflicht: **schnell und scharf**.
     Slint war in den Versuchen des Besitzers unscharf und fehlerhaft, iced und Slint
     gelten als zu eingeschränkt. Im ADR ehrlich bewerten: Aufwand eines eigenen Toolkits (Layout,
     Text mit Shaping und Hinting, Barrierefreiheit, Eingabemethoden), worauf es aufsetzen
     könnte, und woher Unschärfe kommt (Skalierung, Subpixel, Fraktional-Scaling).
   - **Schrift und Icons:** noch offen. Richtung: nicht der typische Linux-Look (kein
     Adwaita/Cantarell). Icons im Stil von macOS (weich, abgerundet, keine eckige
     Material-Optik), aber eigene: Apples Icons, Symbole und SF Pro sind geschützt.
     Vorschlag zum Besprechen: Inter als Systemschrift.

**M3-Plan (vom Besitzer am 17.10. bestätigt):** `m3-plan` sind #14 BTF (Basis), #17
Hardware (NVIDIA, Bluetooth und Ton im Desktop; WLAN und zweite SSD in der Basis), #34
zram/oomd und #35 nftables (beide Basis). #36 fwupd kommt mit M4. Regel des Besitzers:
nichts, was ohne Nutzen Leistung kostet.

**Offen, kein Hindernis für M3**
- #5 bitgleiche Bauten: CI und lokal gleich bis auf `usr/lib/libdevmapper.so.1.02` (LVM2).
  Lokal bitgleich über Pfad, Zeit, 4 CPUs und root. Übrig: Umgebung der CI (bz dort ohne bwraps
  `--clearenv`) oder Host. Nächster Schritt: die Datei als CI-Artefakt, abschnittsweise
  vergleichen. Vergleichsbau unter `~/scratch/`: `git worktree add --detach …`, Quellen aus
  `build/cache` kopieren, `./bz build && ci/digest.sh`.
- #4 vmspawn bewerten, #22 Hardware-Warnungen, #11 PCIe-Retraining mit Kernel 7.3 auf der
  Hardware prüfen, #10 beobachten (seit 14 Läufen nicht), #12/#13 Upstream (sendet der Besitzer).
- ShellCheck in die CI (Paket im Buildroot, mit der nächsten Lock-Änderung bündeln).
- **vhdx verkleinern:** `wsl --manage … --set-sparse true` geht nicht (17.10.: „Sparse VHD
  … wegen möglicher Datenbeschädigung deaktiviert“, `Wsl/Service/E_INVALIDARG`).
  `--allow-unsafe` nicht verwenden. Stattdessen von Hand: in WSL `sudo fstrim -av`, dann
  `wsl --shutdown`, dann `diskpart` als Administrator mit `select vdisk
  file="C:\Users\Joe\AppData\Local\wsl\{4cae8202-1c42-41f8-840d-f8b846346de8}\ext4.vhdx"`,
  `attach vdisk readonly`, `compact vdisk`, `detach vdisk`. Das muss wiederholt werden, wenn
  der Platz wieder wächst. Der `gh`-Token darf keine Workflows starten.

**WSL-Home:** `~/beize`, `~/hw-logs` (Journal und Leerlauf des 0.4-Tests), `~/upstream`
(Kernel-Bäume und Entwürfe für #12/#13 – behalten), `~/scratch` (leer halten).

**Arbeitsweise (vom Besitzer freigegeben):** ADR vorab abstimmen, nichts abschalten „weil
unnötig“, keine fremden Patches. Bei Fehlern zuerst fragen, welche eigene Entscheidung uns auf
einen ungetesteten Pfad gebracht hat. Wegwerf-Arbeit nach `~/scratch/` und am Ende des Schritts löschen.
