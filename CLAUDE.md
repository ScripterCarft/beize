# Beize – Übergabe für Claude Code

Einstieg für jede neue Session; ersetzt den Chatverlauf. Stand: siehe §11. Was im Code,
in den ADRs, im README oder in `docs/` steht, steht hier nicht noch einmal.

## 1. Was Beize ist

Ein eigenes Betriebssystem auf dem Linux-Kernel: **so konsequent wie macOS, so
frei wie Windows, technisch so offen wie Linux.** Kernel, Treiber und
Basisbibliotheken kommen aus Upstream (selbst gebaut), alles darüber –
Compositor, Shell, Einstellungen, Dienste – wird eigener Code (Rust).
**Lies vor größeren Entscheidungen `docs/VISION.md`.** Meilensteine: M1 Bootet in
QEMU ✓ · M2 Echte Hardware ✓ · **M3 Compositor (läuft)** · M4 Alltagstauglich ·
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
- Bei Fehlern zuerst fragen, welche eigene Entscheidung uns auf einen ungetesteten Pfad
  gebracht hat. Nichts abschalten „weil unnötig“, keine fremden Patches; Upstream-Fehler mit
  Bericht und Patch von Claude, gesendet vom Besitzer, ohne Umgehung bis dahin. Behauptungen
  belegen (Belegtes von Erschlossenem trennen, sonst messen). Neue Architektur: ADR vorab
  abstimmen.
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
| `os/` | eigener Rust-Code im Image: `beize-tpm` (initrd: PCR-12-Prüfung, Neubinden nach Recovery, Notfall-Tor mit Neustart einer Version auf Probe, Keymap aus der EFI-Variable), `beize-setup` (1. Start: Tastatur, Konto, Zeitzone), `beize-accounts` (Kontodienst `beize-accountsd`, Konten als systemd-Benutzerdatensätze in `/etc/userdb`; Client-Bibliothek; `beize-account passwd|reset|add`), `beize-update` (ADR 0020: `beize-updated` prüft und installiert über systemd-sysupdate, `beize-update-fetch` holt ohne Rechte, `beize-update install <Quelle>`), `beize-varlink` (Varlink, sd-json, `Secret` für beide Dienste), `beize-vulkan` (ADR 0027 §9: Vulkan-Gerät, Quad-Pipelines mit Shadern, Bilder im Textur-Array; geteilt von Compositor und Beize UI, ohne Smithay), `beize-ui` (ADR 0027 §9: Toolkit der eigenen Apps; Fenster über Vulkan-WSI, sRGB; Text mit Parley, Skrifa, zeno und Glyphen-Atlas; `Offscreen` für Tests; `beize-ui-test` nur Test-Medium, Referenz `tests/sample.png`), `beize-compositor` (ADR 0027: Smithay, eigene Bildschleife und Vulkan-Renderer; `gpu`/`output` Bildschirm, `state` Protokolle und wann gezeichnet wird, `shell` xdg-shell, `windows` schwebende Fenster (Platz, Maximieren, Verschieben/Größe per Grab), `trust`/`layers` die Shell und ihre Leisten, `input`/`keyboard`/`cursor`; Log mit Journal-Priorität, `BEIZE_COMPOSITOR_LOG=debug`), `beize-session` (startet den Desktop aus einer Anmeldung und beendet ihn mit ihr), `beize-systemd` (sd-login, sd-bus zum Benutzermanager; für beize-session und den Compositor, der damit Apps startet), `beize-terminal` (Terminal auf Beize UI, Unit `app-beize\x2dterminal@.service`), `beize-testclient` (Wayland-Client für Tests, nur Test-Medium) |
| `tools/bz/src/` | Build-Werkzeug: `source` `recipe` `packages` `install` `kernel` `layout` `tree` `usr` `initrd` `microcode` `secureboot` `image` `installer` `debuginfo` (MiniDebugInfo je Paket) `stamp` (Fingerabdrücke der Schritte; `build.rs` hasht den Code, der Ergebnisse bestimmt) `boottimes` `hwcheck` `outdated` `paths` `util` (`run`, `say!`/`capture`); `vm/`: `machine` `console` (`Session`) `report` `checks` `run` `debug`, `smoke/` (`mod.rs` Ablauf; Szenarien in `trust` `access` `updates` `integrity` `desktop`) |
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
./bz smoke           # DER Abnahmetest (~2,5 min, Szenarien parallel), vor jedem Commit grün
./bz debug 'journalctl -b -p warning' …   # Befehle auf der Root-Shell einer VM, Ausgabe auf stdout
#   --machine <name> (von smoke liegengelassen) · --tpm <dir> · --recovery-key … · '@wait N'
#   '@screendump build/x.png' (was der Bildschirm zeigt) · '@keys a shift+z ctrl+alt+f2'
#   · '@mouse 40 30 [click]' (QMP; PS/2-Maus, relativ;
#   pixelgenau zieht nur der Desktop-Smoke, über ein virtio-Tablet: `console::drag`) · Testprogramme der Pakete
#   (`usr/lib/beize/test/`, `[files] build`) liegen auf dem Test-Medium: in der VM
#   `mount -o ro /dev/disk/by-id/virtio-beize-test /mnt`, z. B. `/mnt/beize-render-test`
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
- **Desktop von Hand in der VM** (`bz debug`; tty1–6 haben ein getty): `chvt 7`, dann
  `systemd-run --unit=login1 -p PAMName=login -p User=smoke -p TTYPath=/dev/tty7
  -p StandardInput=tty -p StandardOutput=journal -p StandardError=journal
  /usr/bin/beize-session` (die Anmeldung; danach läuft ein Terminal). Zustand:
  `systemctl --user -M smoke@ status beize-compositor`, Log `journalctl -b _UID=1000`.
  Programme als Benutzer-Unit: `systemd-run -M smoke@ --user /run/t/beize-testclient --frames
  600` (Test-Medium nach `/run/t`, die frische Disk hat kein `/mnt`), ebenso `beize-ui-test
  --seconds 30` oder `/usr/lib/beize/beize-terminal`. `@screendump` nur unter `build/`
  (QEMUs Sandbox). Tote Tasten: `localectl set-keymap --no-convert de-latin1 && localectl
  set-x11-keymap --no-convert de pc105 "" terminate:ctrl_alt_bksp`, dann `@keys grave_accent e`.
- Start in der VM: 3,8 s Gast-Uptime, erster Start ~6 s, Smoke lokal ~137 s. Der zweite Start
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
Netzwerk · 0012 Zeit (NTS) · 0013 Was läuft (Units im Image) · 0014 Toolchain x86-64-v3 + Härtung · 0015 Kernel: Konfiguration, Module, Microcode, Firmware · 0016 initrd aus Funktionen, Notfall-Shell, hwdb · 0017 Tastatur (XKB, kbd-Keymaps, Keymap der initrd in EFI-Variable) · 0018 Rechte (run0 + polkit, kein setuid, Wiederherstellung) · 0019 Konten (Beizes Kontodienst, `/etc/userdb`, Rechte durch Beweis; homed verworfen) · 0020 Updates (beize-updated + systemd-sysupdate, ML-DSA-signierte Liste, 4-GiB-Slots) · 0021 Buildroot (gepinnt, abgeschottet, lokal = CI) · 0022 SBOM und Schwachstellen (NVD, RustSec) · 0023 Absturzberichte (MiniDebugInfo, elfutils) · 0024 Bootloader-Konfiguration mit jeder Version · 0025 Varianten `base`/`desktop` (Vorgabe `desktop`, Start ohne Oberfläche) · 0026 TPM-Schlüssel für „(Basis)“ (mit M4) · 0027 Grafik (Smithay, nur Vulkan, eigene Bildschleife, Beize UI, NVK, Inter) · 0028 Shell (eigener Prozess, Menüleiste + Dock, Strg-Modell, Phosphor). Neue Architekturentscheidung = neues ADR; spätere Änderungen
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
  ab (schlechter = zurücknehmen, besser = Wert eintragen). Neuer `beize-*`-Dienst = Eintrag
  (Benutzer-Units unter `[user-exposure]`). Units des Desktops liefert das Paket, das sie
  braucht (`system/usr` landet in jeder Variante).
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
- `curl` ohne `--fail` speichert eine 404-Seite still: Prüfsummen nur von `--fail`-Downloads.
- `bz debug --machine <name>` startet die Disk der liegengelassenen Maschine, nicht den neuen Bau.
- Während eines Baus nie den Zweig wechseln (`bz` liest z. B. `kernel/measure/`); einen langen
  Bau mit einem Hintergrund-Job abwarten, der sich meldet.
- systemd/sd-bus: Eine an sd-event gehängte Verbindung schließt, wenn die Schleife endet;
  nach `StartUnit` ist ein Target noch „inactive“ mit wartendem Auftrag (auf `JobRemoved`
  warten); `ProtectHome=yes` sperrt auch `/run/user`; `systemd-analyze --user` braucht
  `XDG_RUNTIME_DIR` (`bz` misst mit leerer Umgebung, sonst CI ≠ lokal).
- Ein per `kill` geschicktes SIGSEGV verpufft in Rust-Programmen (Handler für Stack-Überlauf):
  Abstürze mit SIGABRT nachstellen.
- Vulkan: Die `Instance` muss das Gerät überleben, sonst stürzt Mesa beim ersten Shader ab.
- Eingaben mit echter Haltezeit nachstellen (QMP `input-send-event`), nicht mit `@keys` (20 ms).

## 10. Offene Punkte

**GitHub Issues** (`gh issue list`; Labels `m3-plan`, `upstream`, `hardware`). Neue Punkte dort
anlegen, nicht hier; ein Commit, der einen Punkt erledigt, nennt `Fixes #N`. Upstream-Entwürfe
liegen in WSL `~/upstream/` (sendet der Besitzer, #13).

## 11. STAND UND NÄCHSTE AUFGABE (2026-10-23: M3 Schritt 4 Shell, (a) und b1 fertig, b2 als Nächstes)

**Beim Besitzer** läuft der Messbau 0.5.101 (Tag `measure/desktop-0.5.101`); 0.6 von `main`
ersetzt ihn (der Updater nimmt nur Neueres). Neu installieren: `keys/secureboot/README.md`,
Update vom Stick und Hardware-Test: `docs/hardware.md`. Wiederherstellung: nach F8
Leertaste halten, „(Recovery)“, Recovery-Key, `s`. Der Stick **BEIZE** bleibt am PC;
Ergebnisse holt Claude per PowerShell (`Copy-Item D:\beize-test\results-…
\\wsl.localhost\Ubuntu-26.04\home\joe\hw-logs\`; in WSL ist D: nicht eingehängt).

**M3 ist modular** (Besitzer, 16.10.; ADR 0025): Basis (alles aus M2, ohne Oberfläche) und
Desktop darüber, Varianten eines Baus wie Windows Server Core oder Fedoras Editionen.
„(Basis)“ startet bis M4 mit dem Recovery-Key (eigener TPM-Schlüssel: ADR 0026, #38).

**Grafik nach ADR 0027, Schritte 1–3 fertig:** NVK und lavapipe (§4), Compositor auf Smithay
mit eigener Bildschleife und eigenem Renderer (§6–8), Beize UI und Terminal (§9). Auf der
3070 Ti: 2560x1440 @ 240 Hz, NVK direkt in den Bildschirmpuffer, 4 von 4797 Bildern spät.
Fenster bleiben bis Schritt 4 bildschirmfüllend, ohne Titelleiste (dem Besitzer so gesagt).

**Upstream, wartet auf den Besitzer** (Entwürfe in `~/upstream`, `main` bleibt ohne fremde
Patches, der Messzweig wendet `kernel/measure/*.patch` an):
- nouveau EDID (240 Hz): am 21.10. gesendet (b4-Tag `sent/20261021-nouveau-edid-segment-…`);
  Antworten leitet der Besitzer weiter.
- nouveau GSP-Interrupts (`IRQDEST`): am 23.10. gesendet (b4-Tag
  `sent/20261023-nouveau-gsp-irqdest-…-v1`).
- Smithay: drei Berichte in `~/upstream/smithay/ISSUES.md`.
- ntpd-rs #2539 (32 Warnungen je Start, offline endlos): am 23.10. kommentiert, Backoff-Patch
  angeboten, Frage nach der Basis (upstream baut den Daemon um). Patch erst nach ihrer Antwort.
  Keine Umgehung in Beize (ADR 0011).

**(h) Aufräumen ist fertig (23.10.):** Befunde des Hardware-Tests belegt (NTS oben; schwarze
Lücken und Zeitplan in ADR 0027 §6), je spätem Bild eine Zeile `late frame`, Doku gestrafft,
Journal von 0.5.101 durchgesehen (alles in Issues: #22, #11, #35; growfs-Fehler kommt bei
jedem Start, `link-volume-key=` braucht eine Sicherheitsabwägung), Messbauten 0.5.98–0.5.100,
`~/hw-test` und `~/scratch` gelöscht. Release 0.5.101 und die Messtags bleiben, solange 0.5.101
beim Besitzer läuft. Tests schneller machen kommt erst nach der Shell (Besitzer).

**Schwarze Lücken (Besitzer, 23.10.)**, belegt aus dem Journal von 0.5.101:
- Start: 1,74 s ohne Bild, weil nouveau den Firmware-Framebuffer vor dem GSP-Start entfernt
  (`nouveau_drm_probe`: „no way to put it back“); kein Beize-Entscheid.
- Wechsel zur Sitzung: der Compositor setzt den Modus (erstes Bild 0,13 s danach), der Rest
  ist das Einrasten des Monitors. Erschlossen, nicht gemessen: die Konsole läuft im
  bevorzugten Modus (fbdev), der Compositor mit 240 Hz. Lösung nach Fedoras flicker-free boot
  (`FRAMEBUFFER_CONSOLE_DEFERRED_TAKEOVER`, die grafische Anmeldung setzt den ersten Modus),
  sinnvoll erst mit grafischer Anmeldung.

**Nächster Hardware-Test** (nicht öfter als nötig, Besitzer): die Zeilen `late frame` (Zeitplan
in kurzen Folgen, ADR 0027 §6), EDID und `/sys/kernel/debug/dri/0/state` auf der Konsole (Modus
der Konsole), Strichstärke hell auf dunkel gegen dunkel auf hell. `measure/hw-test.sh` auf dem
Messzweig dafür erweitern.

**JETZT: Schritt 4 (Shell) nach ADR 0028 (angenommen 23.10.), (a) fertig 23.10.:**
`os/beize-shell` (Menüleiste mit Uhr, Dienst), Compositor `trust.rs` (Shell an ihrer cgroup per
SO_PEERPIDFD/PIDFD_GET_INFO) und `layers.rs` (wlr-layer-shell nur für die Shell, Fenster
darunter), Beize UI `Window::panel`; Desktop-Smoke prüft Menüleiste, Fensterbereich und den
Neustart der Shell. **(b) ist geteilt.** b1 fertig 23.10.: Compositor `windows.rs` (Fenster in
eigener Größe, mittig, Kaskade; Maximieren; Verschieben/Größe per Grab, Randstreifen 5 px),
Terminal öffnet mit 80x24, der Smoke zieht pixelgenau über ein virtio-Tablet (`VIRTIO_INPUT=m`).
**Als Nächstes b2:** Titelleiste in Beize UI (28 px, Titel mittig, Knöpfe rechts mit
Phosphor-Symbolen über resvg nach §8; Ziehen = `move`, Doppelklick = maximieren). Danach b3
xdg-decoration mit Rahmen für fremde Apps (gemeinsames Text-Crate), b4 runde Ecken und
Schatten. Zeigerbilder für die Ränder kommen mit (d). Die Beize-UI-Referenz entsteht offscreen
in der VM: `./bz debug --machine run 'mkdir -p /run/t && mount -o ro
/dev/disk/by-id/virtio-beize-test /run/t' '/run/t/beize-ui-test --offscreen /run/s.ppm 800x600'
'gzip -9c /run/s.ppm | base64 -w0'`, auf dem Host zurück und nach PNG. Entschieden: Shell als eigener Prozess wie macOS,
aber aus einem Guss (Start ohne Flackern); Dock unten, Menüleiste oben; Titelleiste von Beize
UI; Strg-Modell (Super gehört der Shell); Knöpfe rechts; Symbole Phosphor, App-Symbole
eigen; gestaltete Animationen zuletzt (g). Der Lasttest (ADR 0028 §9) ist der Wunsch des
Besitzers. Schrift: sauber, nicht pixelig (Inter). Geht auf Hardware etwas schief: Logs mit `hw-test.sh logs`/`-1`,
erst die Ursache im Quelltext (Mesa, nouveau, Smithay) suchen, dann ändern.

**Grundlegende Vorschläge** (Durchsicht 23.10., je ein ADR, noch nicht entschieden):
- **Delta-Updates** vor Updates übers Netz (M4): Heute lädt jedes Update das ganze Image
  (0.5.101: 185 MB). macOS, Android und ChromeOS liefern Deltas. Die zstd-Komprimierung im
  EROFS macht Binär-Diffs des Images wertlos, daher Stücke des unkomprimierten Inhalts
  (casync/desync-artig oder je Datei), aus denen der Rechner das bitgleiche, signierte Image
  wieder baut (setzt #5 voraus). Zuerst messen: Delta zweier aufeinanderfolgender Bauten.
- **Apps als signierte EROFS-Images mit verity**, auf derselben Vertrauenskette wie `/usr`
  (Vorbild: signierte .app auf macOS' versiegeltem Volume; systemd-sysext/portable services):
  Installieren = eine Datei ablegen, Start ohne Entpacken, Integrität bei jedem Lesen. Für das
  App-Modell vor M4.
- **Tests (nach der Shell, Besitzer):** Von ~11 s je VM-Start sind nur ~5,4 s der Gast; 1–2 s
  TPM-Emulation (#39) und ~3 s unzugeordnet (QEMU, swtpm, Journal-Disk). Das zuerst messen
  und beheben, dann Szenarien ohne Startprüfung aus einem Snapshot der laufenden VM starten.

**M3-Plan** (Besitzer, 17.10.): `m3-plan` sind #14 BTF (Basis), #17 Hardware (NVIDIA,
Bluetooth und Ton im Desktop; WLAN und zweite SSD in der Basis), #34 zram/oomd und #35
nftables (beide Basis). #36 fwupd kommt mit M4.

**Offen, kein Hindernis für M3**
- #5 bitgleiche Bauten: CI und lokal gleich bis auf `usr/lib/libdevmapper.so.1.02` (LVM2).
  Nächster Schritt: die Datei als CI-Artefakt, abschnittsweise vergleichen. Vergleichsbau unter
  `~/scratch/`: `git worktree add --detach …`, Quellen aus `build/cache`, `./bz build &&
  ci/digest.sh`.
- #4 vmspawn bewerten, #22 Hardware-Warnungen, #11 PCIe-Retraining mit Kernel 7.3 prüfen, #10
  beobachten, #12/#13 Upstream (sendet der Besitzer); #39 Firmware-Zeit der VM (TPM-Emulation).
- ShellCheck in die CI (Paket im Buildroot, mit der nächsten Lock-Änderung bündeln).
- **Platz in WSL:** wächst durch `target/` (bei Bedarf `cargo clean`) und Releases in
  `build/out/<v>/release/` (Messbauten nach dem Schritt löschen). **vhdx verkleinern** (schrumpft
  nie von selbst; Sparse-VHD lehnt WSL ab, `--allow-unsafe` nicht verwenden): in WSL `sudo
  fstrim -av`, `wsl --shutdown`, dann `diskpart` als Administrator: `select vdisk
  file="C:\Users\Joe\AppData\Local\wsl\{4cae8202-1c42-41f8-840d-f8b846346de8}\ext4.vhdx"`,
  `attach vdisk readonly`, `compact vdisk`, `detach vdisk`.

**WSL-Home:** `~/beize`, `~/hw-logs` (Journale und Ergebnisse der Hardware-Tests),
`~/upstream` (Kernel-Bäume und Entwürfe – behalten), `~/scratch` (leer halten; Wegwerf-Arbeit,
am Ende des Schritts löschen).
