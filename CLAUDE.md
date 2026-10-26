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
| `os/` | eigener Rust-Code im Image: `beize-tpm` (initrd: PCR-12-Prüfung, Neubinden nach Recovery, Notfall-Tor mit Neustart einer Version auf Probe, Keymap aus der EFI-Variable), `beize-setup` (1. Start: Tastatur, Konto, Zeitzone), `beize-accounts` (Kontodienst `beize-accountsd`, Konten als systemd-Benutzerdatensätze in `/etc/userdb`; Client-Bibliothek; `beize-account passwd|reset|add`), `beize-update` (ADR 0020: `beize-updated` prüft und installiert über systemd-sysupdate, `beize-update-fetch` holt ohne Rechte, `beize-update install <Quelle>`), `beize-varlink` (Varlink, sd-json, `Secret` für beide Dienste), `beize-vulkan` (ADR 0027 §9: Vulkan-Gerät, Quad-Pipelines mit Shadern – `quad.frag` jedes Bild, `backdrop.frag` Hintergrund und Blur –, Bilder im Textur-Array; geteilt von Compositor und Beize UI, ohne Smithay), `beize-draw` (was Beize UI und Compositor gleich zeichnen: Szene aus Quads, Text mit Parley, Skrifa, zeno, Symbole (Phosphor, resvg) im Glyphen-Atlas, Farbbilder (App-Symbole, PNG/SVG) im Farbatlas, `icon` die App-Symbole von Beize, Titelleiste `titlebar`, ADR 0028 §2; ohne Wayland und GPU), `beize-ui` (ADR 0027 §9: Toolkit der eigenen Apps; `App` hält Verbindung, Eingabe, GPU, Atlas und Schrift für alle Fenster einer App (`app`), Fenster über Vulkan-WSI, sRGB, mit der Titelleiste aus `beize-draw` (`window`), Leisten der Shell, auch durchsichtig; `desktop` die Fensterliste für die Shell; `Offscreen` für Tests; `beize-ui-test` nur Test-Medium, Referenz `tests/sample.png`), `beize-compositor` (ADR 0027: Smithay, eigene Bildschleife und Vulkan-Renderer; `gpu` die GPU, Renderer und ihr Bildschirm (an-/abstecken über udev, `link-status`, ADR 0029), `output` ein Bildschirm und seine Bildschleife, `state` Protokolle und wann gezeichnet wird, `scene` was ein Bild zeigt (vorn nach hinten) und sein lebendiges Glas, `shell` xdg-shell, `windows` schwebende Fenster (Platz, Maximieren, Verschieben/Größe per Grab), `decorations` Rahmen fremder Apps (xdg-decoration, Leiste als eigenes Bild über `VkRenderer::draw_picture`), `trust`/`layers` die Shell und ihre Leisten, `shell_v1` ihre Fensterliste (beize-shell-v1; Minimieren und Zurückholen in `windows`), `backdrop` Hintergrund, Grundglas und Erscheinungsbild, `glass` wo die Shell Glas will (`beize_glass_v1`), `input`/`keyboard`/`cursor` (Zeiger aus dem Thema, cursor-shape-v1); Log mit Journal-Priorität, `BEIZE_COMPOSITOR_LOG=debug`), `beize-session` (startet den Desktop aus einer Anmeldung und beendet ihn mit ihr), `beize-systemd` (sd-login, sd-bus zum Benutzermanager; für beize-session; `apps` startet Apps als `app-<ID>@<Zufall>.service`, für Compositor und Dock), `beize-protocols` (eigene Wayland-Protokolle aus XML, `client`/`server`: `beize-shell-v1.xml`), `beize-shell` (Menüleiste mit App-Name und Uhr, `dock`, `apps` Namen und Symbole aus Desktop-Dateien, hicolor), `beize-terminal` (Terminal auf Beize UI, Unit `app-beize\x2dterminal@.service`), `beize-testclient` (Wayland-Client für Tests, nur Test-Medium) |
| `tools/bz/src/` | Build-Werkzeug: `source` `recipe` `packages` `install` `kernel` `layout` `tree` `usr` `initrd` `microcode` `secureboot` `image` `installer` `debuginfo` (MiniDebugInfo je Paket) `stamp` (Fingerabdrücke der Schritte; `build.rs` hasht den Code, der Ergebnisse bestimmt) `boottimes` `hwcheck` `outdated` `paths` `util` (`run`, `say!`/`capture`); `vm/`: `machine` `console` (`Session`) `report` `checks` `run` `debug`, `smoke/` (`mod.rs` Ablauf; Szenarien in `trust` `access` `updates` `integrity` `desktop`) |
| `recipes/*.toml` | ein Rezept pro Paket: Quelle (gepinnt + signiert, `latest` für `bz outdated`), `deps`, `stage`, `[build] script`, zuletzt die Metadaten `[sbom]` (Lizenz, CPE, ADR 0022) und `[files]` (was ins Image kommt); ohne `stage` nur Quelle (linux, linux-firmware, intel-microcode) |
| `keys/` | Maintainer-Schlüssel + `README.md`; `microsoft/` (gepinnt); `secureboot/` öffentliche Produktionsschlüssel + Anleitung für den Besitzer |
| `images/` | je Variante ein Ordner (ADR 0025, `bz`: `variant.rs`): `base/`, `desktop/` (erweitert `base`, fügt nur hinzu) mit `variant.toml` Name und Pakete für `/usr` · `system.preset`/`user.preset` aktivierte System-/Benutzer-Units · `running-units.txt` was im Smoke-Test laufen muss (ADR 0013) · `security.toml` gemessene Härtung der Dienste · `sizes.toml` Größe von `/usr` und den eigenen Programmen; eine Datei ohne Ergänzung fehlt beim Desktop. Gemeinsam: `initrd.toml` Funktionen und Module der initrd (ADR 0016, Auswahl in `bz`: `select/`) |
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
./bz smoke           # DER Abnahmetest (~2,3 min, Szenarien parallel), vor jedem Commit grün
./bz debug 'journalctl -b -p warning' …   # Befehle auf der Root-Shell einer VM, Ausgabe auf stdout
#   --machine <name> (von smoke liegengelassen) · --tpm <dir> · --recovery-key … · '@wait N'
#   · --screen 2560x1440 (Bildschirmgröße der VM; ohne: 1280x800 wie Smoke und Referenzen)
#   '@screendump build/x.png' (was der Bildschirm zeigt) · '@keys a shift+z ctrl+alt+f2'
#   · '@mouse 40 30 [click]' (QMP; PS/2-Maus, relativ;
#   pixelgenau zieht nur der Desktop-Smoke, über ein virtio-Tablet: `console::drag`) · Testprogramme der Pakete
#   (`usr/lib/beize/test/`, `[files] build`) liegen auf dem Test-Medium: in der VM
#   `mount -o ro /dev/disk/by-id/virtio-beize-test /mnt`, z. B. `/mnt/beize-testclient`
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
  /usr/bin/beize-session` (die Anmeldung; Terminal mit `@keys ctrl+alt+t`). Zustand:
  `systemctl --user -M smoke@ status beize-compositor`, Log `journalctl -b _UID=1000`.
  Programme als Benutzer-Unit: `systemd-run -M smoke@ --user /run/t/beize-testclient --frames
  600` (Test-Medium nach `/run/t`, die frische Disk hat kein `/mnt`), ebenso `beize-ui-test
  --seconds 30` oder `/usr/lib/beize/beize-terminal`. `@screendump` nur unter `build/`
  (QEMUs Sandbox). Tote Tasten: `localectl set-keymap --no-convert de-latin1 && localectl
  set-x11-keymap --no-convert de pc105 "" terminate:ctrl_alt_bksp`, dann `@keys grave_accent e`.
- Start in der VM: 3,8 s Gast-Uptime, erster Start ~6 s, Smoke lokal ~140 s (5 VMs parallel, eine VM-Breite CPU frei, `parallel_vms`). Der zweite Start
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
Netzwerk · 0012 Zeit (NTS) · 0013 Was läuft (Units im Image) · 0014 Toolchain x86-64-v3 + Härtung · 0015 Kernel: Konfiguration, Module, Microcode, Firmware · 0016 initrd aus Funktionen, Notfall-Shell, hwdb · 0017 Tastatur (XKB, kbd-Keymaps, Keymap der initrd in EFI-Variable) · 0018 Rechte (run0 + polkit, kein setuid, Wiederherstellung) · 0019 Konten (Beizes Kontodienst, `/etc/userdb`, Rechte durch Beweis; homed verworfen) · 0020 Updates (beize-updated + systemd-sysupdate, ML-DSA-signierte Liste, 4-GiB-Slots) · 0021 Buildroot (gepinnt, abgeschottet, lokal = CI) · 0022 SBOM und Schwachstellen (NVD, RustSec) · 0023 Absturzberichte (MiniDebugInfo, elfutils) · 0024 Bootloader-Konfiguration mit jeder Version · 0025 Varianten `base`/`desktop` (Vorgabe `desktop`, Start ohne Oberfläche) · 0026 TPM-Schlüssel für „(Basis)“ (mit M4) · 0027 Grafik (Smithay, nur Vulkan, eigene Bildschleife, Beize UI, NVK, Inter) · 0028 Shell (eigener Prozess, Menüleiste + Dock, `beize-shell-v1`, Strg-Modell, Phosphor) · 0029 Bildschirme (Hotplug, link-status, GPU und Bildschirme getrennt). Neue Architekturentscheidung = neues ADR; spätere Änderungen
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
  Parallele VMs nach CPUs/RAM (je 4 CPUs und 3 GiB reserviert, Gast 1 GiB, Desktop 1,5 GiB).
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
- **Größe als Ratsche:** `images/<variante>/sizes.toml` hält `/usr` (EROFS) und jedes eigene
  Programm in KiB fest; `bz` bricht bei mehr als 3 % Abweichung ab (größer = zurücknehmen oder mit
  Grund im Commit erhöhen, kleiner = Wert eintragen). Ein neues eigenes Programm = Eintrag.
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
- **Schichten der eigenen Crates** (`deny.toml`, `[bans]`): wer welche Crate nutzen darf (Smithay nur
  der Compositor, Wayland-Client nur Beize UI …); ein neuer Nutzer ist eine Entscheidung mit Grund
  dort, `cargo deny` (CI) lehnt jeden anderen ab.
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
- **WSL-Kernel ohne Preemption** (`CONFIG_PREEMPT_NONE`): Free Page Reporting an Hyper-V
  (`page_reporting_process`, alle 2 s) hielt eine CPU bis 141 ms, wenn VMs RAM freigaben – Gäste
  standen still, Zeitprüfungen des Desktops scheiterten (belegt 25.10. per SCHED_FIFO-Wächter und
  SysRq-l-Backtraces). Abhilfe `C:\Users\Joe\.wslconfig`: `[wsl2] kernelCommandLine = preempt=full`
  (danach `wsl --shutdown`; sofort und bis zum Neustart: `echo full | sudo tee
  /sys/kernel/debug/sched/preempt`). `bz smoke` warnt, solange die Kommandozeile es nicht setzt.

## 10. Offene Punkte

**GitHub Issues** (`gh issue list`; Labels `m3-plan`, `upstream`, `hardware`). Neue Punkte dort
anlegen, nicht hier; ein Commit, der einen Punkt erledigt, nennt `Fixes #N`. Upstream-Entwürfe
liegen in WSL `~/upstream/` (sendet der Besitzer, #13).

## 11. STAND UND NÄCHSTE AUFGABE (2026-10-26: Messbau 0.5.106 getaggt – Besitzer testet; danach Befund 2 beheben, Befund 1 bestätigen, E)

**Messbau 0.5.105 gemessen (26.10., `~/hw-logs/results-20261026-105219`, Tag
`measure/desktop-0.5.105`, Zweig gepusht).** Lasttest (12 Fenster, 240 Hz): CPU je Bild Median
1,48 → **0,70 ms** (busy 43 → 27 %), Glas 0,77 → **0,00**, 112 032 von ~115 200 Bildern; Teile
(Median/99 %): damage 0,34/0,66, record 0,18/0,34, sync 0,07, queue 0,05, scene 0,05, wait 0,00.
Testfenster 4805 Bilder, 2 spät. Spät im Lasttest 193 von 6977, fast alle „display“ (C c).
Besitzer 26.10.: 112 032 von 115 200 ist „erstmal schlecht“ – Ziel **0 späte Bilder** im Lasttest
(jedes späte steht 8,3 statt 4,2 ms, ein Ruckler): die „display“-Späten (C c, fertig vor dem vblank
und doch zu spät; nouveau-Vorlauf, `MOST_LEARNT`) kommen gleich nach den zwei Befunden unten.
Folgerungen: **C endgültig unnötig** (`wait` 0,00 auf NVK); **B kein CPU-Hebel** (sync 0,07 ms) –
Besitzer: trotzdem umsetzen, wenn es die Umsetzung sauberer macht; größter Rest ist `damage`
(Smithays Damage-Tracker über die Elemente). Auffällig: `record` 0,35–0,46 ms in den Läufen mit
Terminal/Ziehen (Verdacht, unbelegt: `shared.collect()`/`dmabufs.retain` am Anfang von `submit`
zählt zu record – mit dem Befund 1 unten zerstörte er hunderte Bilder).

**ZUERST beheben (Besitzer: der neue Chat fixt, „fix directly“):**
1. ~~Resize staut das Terminal, bis es abstürzt~~ **behoben 26.10. (e067f10), Hardware offen.**
   Ursache (nicht der Blocker): Beize UI zeichnete jedes Configure sofort, jede Größe eine neue
   Swapchain mit frischen Puffern (Mesas FIFO bremst da nicht); seit wp_fifo gilt ein Commit je
   Bild → die Größen einer 1000-Hz-Maus stauten sich, jeder Commit hielt seinen Puffer, bis die
   Texturplätze (~977) voll waren. Dazu ack beim Empfang → das letzte ack mit Puffer alter Größe.
   Fix wie GTKs Frame-Clock: Frame-Callback je Present, Configure wartet darauf, ack bei Übergabe.
   Smoke `fast_resize` (1-ms-Schritte): vorher 988 statt 800 px, jetzt 20 ms. Hardware-Test liest
   `resized … ms after the button was released`. Erschlossen (nicht belegt): gescheitert ist der
   Compositor an **offenen Dateien** (Soft-Limit 1024; je dmabuf ein fd), nicht am Texturfeld
   (4096). **Härtung 26.10. (ab8987e, freigegeben):** `LimitNOFILE=524288` (Mutter/KWin/Sway;
   Smoke prüft, Gegenprobe 1024 rot), `held:` zählt offene Dateien (Hardware-Test belegt die
   Ursache), Texturplatz erst beim ersten Zeigen (Weston). Bekannt und **bewusst so gelassen**
   (Besitzer 26.10.: was kein anderer, nicht einmal Weston, macht, hat einen Grund – nichts
   Eigenes): ein Client kann den Compositor weiter erschöpfen (Smithays Warteschlange je Surface
   unbegrenzt, commit-timing in ferne Zukunft, liegengelassene Puffer); kein Budget je Client.
   **Die späten Bilder (112 032 von ~115 200, „display“) macht ein anderer Chat** – hier nicht.
2. **Monitor aus/an: Bild bleibt schwarz.** Belegt: `screen DP-1 is gone` (377,25 s), zurück
   `2560x1440 at 240 Hz, it had none` (384,66), Smithay-Warnung `Failed to destroy old mode
   property blob` (Bericht Nr. 4 in `~/upstream/smithay/ISSUES.md`), „first frame on the screen“
   (Modus-Commit 81 ms), danach keine `pacing:` (Mausbewegungen allein machen keine), obwohl der
   Besitzer blind Maus und Tastatur nutzte. **Widerlegt (26.10.):** „first frame“ steht im
   vblank-Handler – KMS zeigte das erste Bild, das Ereignis kam; was danach kam, ist unbekannt.
   **Messpunkte 64142a6:** nach jedem Moduswechsel 4 Commits/vblanks mit Anschluss- und
   Link-Zustand, Warnung „no vblank … after its last commit“. Nächster Hardware-Test (mit B):
   Besitzer notiert, was der Monitor zeigt (kein Signal / schwarz beleuchtet / Standby), und
   wechselt bei Schwarz auf eine Textkonsole und zurück (`resume()` = anderer Modeset-Weg; Bild
   wieder da → Fehler in unserem Wiederkommen-Pfad). Kabel ab/an nicht gemacht.
   Kernel-Journal nachgeholt (`~/hw-logs/results-20261026-111003`, `hw-test.sh -1`; im Test
   selbst scheiterte run0 mit „Access denied“): **der Kernel meldet von 370–420 s nichts** – kein
   nouveau-/DRM-Fehler, keine Zeitüberschreitung; spricht für die Seite des Compositors
   (Smithays Zustand nach dem Modeset, ein nicht behandeltes Flip-Ereignis), belegt ist es nicht.
   VM-Smoke `screen_comes_back` ist grün
   (virtio-gpu) – Hardware-Fehler, Kandidaten: nouveau-Event nach Modeset aus „aus“, Smithays
   Blob-Zustand.
**B (linux-drm-syncobj) zurückgestellt, Besitzer 26.10.:** seine Bedingung („wenn es sauberer
macht“) ist nicht erfüllt – zweiter Weg neben der impliziten Synchronisation (die bleibt für
Clients ohne das Protokoll), Smithay gibt den Release-Punkt auf der CPU beim Ersetzen frei (unser
Renderer müsste ihn an seinen GPU-Fence koppeln wie Mutter/KWin), Nutzen 0,07 ms. Heute korrekt:
jedes Bild hängt seinen Fence an die gelesenen Client-Puffer (`render/sync.rs`). Wieder aufnehmen,
wenn ein Treiber ohne implizite Synchronisation kommt.
**Messbau 0.5.106 gebaut und geprüft (26.10.):** `measure/desktop` auf `main` gehoben (aeac4a2,
Tag `measure/desktop-0.5.106`, Smoke grün, Skript in der VM ganz durchgelaufen: Spur nach dem
Moduswechsel da, `held: … 41 open files` = Grundzahl der VM). Teil 5 schnell ziehen + `resized …`,
Teil 6 mit Beobachtung und Konsolenwechsel (Strg+Alt+F2/F1). Gepusht werden `main` und der Tag,
sobald CI-Lauf 40304913614 grün ist (Hintergrund-Job); der Messzweig braucht Force-Push
(Rechteprüfung verweigert) → Besitzer: `git push --force-with-lease origin measure/desktop`.
**Beim Besitzer:** Release bauen/signieren, dann Stick → Test (docs/hardware.md A–C).
Danach: Befund 2 mit den Daten beheben, Befund 1 bestätigen, E, dann idle-inhibit; D mit der Shell.
`.wslconfig` `preempt=full` wirkt (Kommandozeile), `bz smoke` warnt nicht mehr (Gegenfall belegt).
**Leerlauf-Ratsche (offen):** der Haupt-Thread des Compositors wachte in 3 von 27 Läufen einmal in
3 s auf, ohne zu zeichnen (25.10. vor, 26.10. nach A; Häufigkeit nicht belegt verschieden). Die
Prüfung nennt jetzt Thread und Journal des Fensters (`BEIZE-WOKE`, `BEIZE-JOURNAL`) und zählt
stoppende Test-Units mit; beim nächsten Rot daraus die Ursache. Debug-Log taugt nicht (verlangsamt,
Lasttest wird rot). Kandidaten ohne Beleg: ein Eingabe-Ereignis ohne Wirkung, ein später Flip.

**Beim Besitzer:** Messbau 0.5.105 (Tag `measure/desktop-0.5.105`); `main` ist 0.6 und ersetzt
ihn (der Updater nimmt nur Neueres). Neu installieren: `keys/secureboot/README.md`; Update vom
Stick und Hardware-Test: `docs/hardware.md` (`measure/hw-test.sh`, 6 Teile, liegt auf dem
Messzweig `measure/desktop`, je Messbau auf `main` gehoben, Version in `tools/bz/src/layout.rs`).
Hardware-Tests nicht öfter als nötig (Besitzer). Wiederherstellung: nach F8 Leertaste halten,
„(Recovery)“, Recovery-Key, `s`. Der Stick **BEIZE** bleibt am PC; Ergebnisse holt Claude per
PowerShell (`Copy-Item D:\beize-test\results-… \\wsl.localhost\Ubuntu-26.04\home\joe\hw-logs\`;
in WSL ist D: nicht eingehängt). Beize hat kein debugfs; den Modus der Konsole schreibt der
Compositor (`screen …: … Hz, it had …`).

**Frühjahrsputz (B), 25.10.:** WSL 66 → 29 GB (`target/`, JetBrains-Cache, alte Releases, Reste
von `bz debug`, Journal); `beize-render-test` entfernt (lief in keinem Test), der Compositor hat
keine lib-Crate mehr; ~50 Entwickler-Werkzeuge aus dem Image (`[files] build` mit Grund); Bibata
nur 24/48 px (27 → 1,9 MB); Test-VMs 1 GiB, Desktop 1,5 GiB statt 2 GiB fest (gemessen,
`vm/machine.rs`); Smoke löscht seine Screendumps nach Erfolg; Terminal wartet auf POLLOUT statt
im 10-ms-Takt; KMS bekommt die Damage des Bildes (FB_DAMAGE_CLIPS); dev-Profil ohne
Debug-Info fremder Crates; Teil 6 (Fotos) aus `hw-test.sh`; Doku gestrafft.
Offen beim Besitzer: Löschen auf der Windows-Seite (alte Claude-Sitzungen dieses Projekts,
Ergebnisse auf dem Stick – Claudes Rechteprüfung verweigert es, der Besitzer führt den Befehl
aus); vhdx verkleinern (unten); Fragen: `~/upstream/mei-src`, `nouveau-edid`, `build-usb`
(3,6 GB, sind mei/usb erledigt?), Zweig `measure/nouveau-edid`, `~/hw-logs/journal`.
Nicht gemacht, Vorschläge: Rust-Programme mit `cargo bloat` messen (Compositor 7,2 MB, Shell
und Terminal je 4,5 MB, der Text-Stapel steckt in jedem), `panic = "abort"` prüfen;
Zeile-für-Zeile-Durchsicht von `windows.rs`, `state.rs`, `input.rs`, Shell, Beize UI `app.rs`,
`smoke/desktop.rs` (bisher nur nach Mustern gesucht). Smithay bringt `rand`, `sha2 0.10`,
`rustix 0.38` fest mit (nicht abschaltbar).

**Phase 3 (Architektur, Performance, Engineering; Besitzer, 25.10.: „alles umsetzen“).**
Ausgangswerte 25.10.: Smoke 152 s, 63 VM-Starts; Desktop im Leerlauf 0 Aufwachvorgänge; `/usr`
168 MB; Compositor 7,4 MB, Shell 4,6, Terminal 4,8. Erledigt (25.10., je gemessen, mit Gegenprobe):
- Smoke 152 → ~140 s, gleiche Prüfungen: Bildschirm-Abfragen sofort und alle 100 ms
  (`poll_screen`), Lasttest 6 s, RAM-Reserve je VM nach Messung, `-nodefaults`, Journal-Disk auf
  eigenem PCI-Slot (Firmware versuchte jedes Mal, von ihr zu booten). 6 VMs gäben 128 s, aber
  RCU-Stalls bis 21 s im Gast (#41) – eine VM-Breite CPU bleibt frei.
- Lebendiges Glas zeichnet in dieselben Bilder (vorher 6 neue VkImages je Änderung): in der VM
  ohne Unterschied, auf der GPU Kernel-Aufrufe – **der Hardware-Test misst es** (Glas war dort
  0,77 von 1,48 ms CPU je Bild).
- Genaue GPU-Zeit je Bild, auch in jeder „late frame“-Zeile (vorher die eines früheren Bildes).
- Schriften per mmap (geteilt statt 1,5 MB je App); `scene.rs` aus `State::render`;
  Desktop-Smoke in Phasen (339 statt 760 Zeilen).
- Regeln, automatisch: Crate-Schichten in `deny.toml`, Leerlauf-Ratsche im Desktop-Smoke (fand
  einen Testclient, der mitten in späteren Prüfungen selbst endete), Größen-Ratsche
  `sizes.toml`, `bz` räumt Platz selbst auf (`space.rs`).
- Verworfen mit Grund: gemeinsamer Journal-Logger; `panic = "abort"` (−1,1 MB, aber ein Panic
  eines Hilfs-Threads beendete die Sitzung); Resize ohne `device_wait_idle` (kein gemessenes
  Problem; falls der Hardware-Test Ruckeln zeigt: `VK_EXT_swapchain_maintenance1`-Fences);
  Intel/AMD brauchen erst Kernel-Treiber, Firmware, libdrm → #17.
**JETZT, in dieser Reihenfolge (Besitzer, 25.10., alles freigegeben):**
1. ~~ADR 0029 Bildschirme~~ erledigt 25.10. (8039442 + Folgecommit „starts without a
   screen“): GPU und Bildschirm getrennt, udev, Neutraining mit `link-status` GOOD; ohne
   Bildschirm beim Start wartet der Compositor (READY erst mit dem ersten). Smoke: Anmeldung
   ohne Bildschirm (`sign_in`), `screen_comes_back` zweimal, je mit Gegenprobe. Hardware-Teil
   „Monitor aus/an, Kabel ab/an“ in `measure/hw-test.sh` kommt mit dem Messbau (4.).
   Smithay-Bericht Nr. 4 (Blob 0) liegt in `~/upstream/smithay/ISSUES.md`. CI beider Commits
   grün (8039442 nach Neustart durch den Besitzer: Bootbudget war Runner-Rauschen).
2. **Compositor unter der Lupe, Stand 25.10. abends** (Besitzer: „production ready, komplette
   Lösungen, keine 10 %“). Erledigt: (i) Smoke-Aussetzer (unten); Befund 1 behoben (711b5a6,
   dmabuf-Commit wartet auf den Fence des Clients, DmabufBlocker wie anvil/Mutter/KWin; vorher
   wartete unsere Submission auf der GPU und `current()` ohne Zeitlimit auf der CPU – ein hängender
   Client fror die Sitzung ein; in der VM nicht prüfbar, shm); Teile je Bild aufgeschlüsselt
   (bdaefd2: damage/wait/record/sync/queue statt „frame“; VM-Lasttest damage 0,13–0,16 ms).
   **Besitzer 26.10.: alles freigegeben.** ~~(A) lebendiges Glas in einem Durchgang~~ erledigt
   (a833702: das Bild selbst kopiert und blurt an der Stelle des Glases, Vergleich der Elemente
   dahinter statt zweitem Damage-Lauf; VM-Lasttest Glas 2,6–3,5 → 0,01 ms; Smoke-Prüfung
   „Glas kehrt zur Farbe ohne Fenster zurück“, 59bcbaf, Gegenproben rot). (B) explizites
   Sync `linux-drm-syncobj-v1` (Smithay hat es; statt 2 ioctls je Client-Puffer und Bild); (C)
   Bild-Slots ohne Blockieren (`current()` wartet unbegrenzt im Haupt-Thread); (D) Direct Scanout
   für Vollbild; (E) Knöpfe als SDF im Quad-Shader (kein SVG), Titeltext fremder Fenster von der
   Shell (wie GNOMEs `mutter-x11-frames`; Compositor ohne Text-Stapel, −1,9 MB), Symbole als
   Phosphor-Schrift (wie SF Symbols). Der nächste Messbau zeigt `sync`/`queue`/`wait` auf NVK.
   Ursprüngliche Notiz (ii) **den Compositor unter die Lupe** (Besitzer: „kein weiterer Compositor, ein richtig
   guter, schneller“): Größe und Arbeit je Bild. Gemessen 25.10. (a691064, gestrippt 7,68 MB,
   `.text` 5,7 MB): Text-Stapel nur für fremde Titelleisten ~1,3 MB (harfrust 376 KiB, skrifa
   320, read-fonts 271, zeno 187, parley 132, fontique 44), SVG für die Knopf-Symbole ~0,6 MB
   (usvg 213, tiny-skia 248, resvg 75, roxmltree 46, svgtypes 41), Abwicklungstabellen 0,58 MB
   (`.eh_frame` + `.gcc_except_table`, panic=unwind bleibt), PIE-Relokationen 0,38 MB
   (Härtung bleibt); Smithay 780, wayland-server 426, eigener Code 499. Wachstum je Commit seit
   9801fb4: ADR 0029 +69 KB, presentation +67, fifo/commit-timing +86, viewporter/activation +93.
   Ratsche verschärfen: jedes Wachstum über ~0,5 % muss den Wert mit Grund eintragen (heute
   rutschten vier Commits je knapp unter 3 % durch). Dann `cargo bloat`, Text/SVG im Compositor
   (vorgerasterte Symbole? gemeinsamer Text-Dienst?), `lto = "fat"`; und vor allem CPU je Bild
   (C b–d) – Größe auf der Platte kostet kaum Laufzeit, Arbeit je Bild schon. (iii)
   **idle-inhibit + Bildschirm aus bei Inaktivität** (gehören zusammen: inhibit wirkt erst mit
   der Leerlauf-Erkennung; Zeiten wie macOS) – Aufteilung des Besitzers, 25.10. Je eine
   Smoke-Prüfung mit Gegenprobe (Testclient bindet sie). Bildschirm aus passt zu ADR 0029
   (Bildschleife ruhen lassen wie bei „weg“, CRTC aus per DPMS/ACTIVE=0). In der VM zählt eine
   Prüfung nur mit Gegenprobe, die rot wird (25.10.: eine ging grün durch, weil Clients ohnehin
   neu zeichneten). Mehrere Testclient-Fenster: `placed(…, "beize-testclient", n)` zählt alle
   (der Rahmen-Client ist jetzt das 5.). Vor dem Commit drei volle Smoke-Läufe: Fehler, die 1 von
   120 Bildern treffen, zeigen sich erst so.
   ~~viewporter + xdg-activation~~ erledigt 25.10.: viewporter nur das Global (Smithays
   Surface-View, der Renderer kann src/Skalierung schon, linear gefiltert). `activation.rs`: ein
   Token zählt nur vom aktiven Client mit Serial ab dem letzten Tastatur-Enter, und nur solange
   dieser Fokus besteht (niri/anvil, Mutters user time; **kein Zeitlimit**, Fokuswechsel beendet
   es), einmal verwendbar; abgelehnt nur ins Log – Dock-Hüpfen wie macOS kommt mit Schritt 4.
   Neue Fenster werden weiter immer aktiv (Focus-Stealing-Schutz für neue Fenster bräuchte Tokens
   beim Start über Dock/Ctrl+Alt+T, `XDG_ACTIVATION_TOKEN`; dann mit ADR). Smoke: `viewport`
   (480×360, Ausschnitt bei ¾, Nähte ±1 px), `activation` (gewährt / Fokus gewandert / ohne
   Serial; Roundtrip statt Timer, Ziel immer das Fenster ohne Fokus). Gegenproben rot: Renderer
   ignoriert src, alles gewähren, nur ohne Serial gewähren, alles ignorieren. `sizes.toml`
   Compositor 7274 → 7569 (davon gemessen viewporter +37, activation +56 KB; Rest seit 9801fb4
   angesammelt, je knapp unter 3 %).
   ~~fifo + commit-timing~~ erledigt 25.10. (`timing.rs`): fifo-Barriere einer gezeigten Fläche
   fällt beim vblank ihres Bildes (`Shown.fifo`), die einer nicht gezeigten **sofort beim
   Zeichnen** (Protokoll: „may ignore the constraint“, gedrosselt wird über die Zeitstempel des
   Clients). Zeitstempel werden **im Zeichenlauf** gegen den vblank freigegeben, auf den das Bild
   zielt (`Output::aim`, wie niri); ein Timer weckt eine Bildzeit vor dem vblank. Mesa 26.2.5
   nutzt im Vulkan-FIFO beides automatisch (je Present: Zeitstempel = nächster vblank − 0,5 ms,
   Barriere, dazu ein leerer Commit mit wait_barrier) – also **Beize UI und das Terminal**.
   Befunde unterwegs: verdeckte Barrieren nur 1×/s → Mesa blockiert im Present, App friert samt
   Schließen-Knopf ein; Barrieren in `frame_done` gegen das letzte Bild → das erste Bild eines
   neuen Fensters galt als verdeckt und wurde verworfen; Timer entfernen → calloop-Warnung.
   Smoke: fifo 120/120, 0 verworfen, vblank-Abstand (`MISSED_VBLANKS` = 2 mit Grund); timed 29/29,
   **0 vor ihrer Zeit**. Gegenproben rot: fifo `unmanaged` (118 verworfen); Zeitstempel
   ignoriert = sofort wecken **und** alles freigeben (29 vor ihrer Zeit, −25 ms) – nur die
   Freigabe-Grenze weg blieb grün, weil dann der Weck-Timer allein den Zeitpunkt trägt; commit-timing
   `unmanaged` taugt nicht (niemand nimmt den Zeitstempel ab, Mesas nächster ist ein
   Protokollfehler, das Terminal stirbt). **Offen, erst messen:** Tipp-Latenz
   von Beize UI im FIFO (Mesa blockiert im Present) gegenüber `FIFO_LATEST_READY`/`MAILBOX`.
   ~~presentation-time~~ erledigt 25.10.: `State::take_shown` nimmt beim Zeichnen Feedback
   **und frame-Callbacks** der gezeigten Flächen (Zustände des Damage-Trackers), `present`
   beantwortet beide beim vblank (Vsync/HwClock/HwCompletion, Sequenz) oder, bei „unverändert“,
   eine Bildzeit später ohne Flags (niri). Befund dabei, belegt mit Zeitstempeln: „frame done“
   ging beim vblank an alle sichtbaren Flächen, auch für einen Commit, der erst nach Zeichenbeginn
   kam → der Client zeichnete sofort das nächste, das ungezeigte wurde verworfen (1 von 120, drei
   Läufe von drei). Jetzt wie Weston (Callbacks beim Zeichnen nehmen, beim vblank beantworten);
   verdeckte/minimierte Flächen höchstens alle `HIDDEN_FRAMES`. Nebenbei „change to screen“ im
   Frames-Test 29,8 → 16,1 ms. Smoke: „120 of 120 shown, 0 discarded“, Flags, Uhr 1, steigend;
   Gegenprobe (vblank ohne Flags) rot.
3. **Alte Tests aufräumen** (Besitzer: „alte Tests wegmachen, die mittlerweile sowieso
   funktionieren“): Prüfungen, die nur einen längst behobenen Einzelfehler nachstellten und keine
   Eigenschaft mehr absichern, sowie Testprogramme/Mess-Skripte ohne Nutzer. Zuerst die Liste mit
   Grund vorlegen; Sicherheit, Boot, Update, Rollback und Recovery bleiben vollständig abgedeckt.
4. Messbau für den Hardware-Test (Glas, C b–d, Monitor aus/an), dann **weiter mit der Shell**
   (Schritt 4 (e)–(g), ADR 0028). Bewusst so bleiben: eigener
Renderer/Bildschleife, `bz`, EROFS zstd, Shell als Prozess, FFI zu libsystemd,
Spec-Konstanten im Smoke.

**C – die Messwerte** (Besitzer, 25.10.: „der nächste Chat fixt alles von meinem Test“).
Jede erst belegen (Ursache im Quelltext und in den Zahlen), mit Vorbild (KWin/Mutter/macOS), dann
ändern, Smoke + Gegenprobe:
a) **Entschieden 25.10.:** die grafische Anmeldung (Compositor ab dem Start, ein Moduswechsel
   im Startvorgang wie beim Mac) kommt später sauber mit eigenem ADR; bis dahin bleibt die Lücke.
   **Konsole 60 Hz, Sitzung 240 Hz** (`it had 2560x1440 at 60 Hz`): der Moduswechsel zur Sitzung
   lässt den Monitor neu einrasten = die schwarze Lücke. Lösung macOS-artig ohne Flackern
   (Fedoras flicker-free boot mit grafischer Anmeldung, oder die Konsole gleich im Modus des
   Desktops; recherchieren, wie Mutter/KWin den vorhandenen Modus übernehmen). Mit dem Besitzer
   abstimmen, falls es die grafische Anmeldung vorzieht.
b) **CPU-Zeit je Bild** (0.5.102 Lasttest: Median 1,44 ms, 99 % 2,88, „busy 42 %“; Ziel „der
   Compositor langweilt sich“): `pacing:` und `late frame` (`scene/glass/frame/commit`)
   auswerten, den größten Teil im Quelltext verfolgen (Smithay-Damage über ~40 Elemente,
   `render_elements_from_surface_tree`, lebendiges Glas, dmabuf-Fences/`foreign`-Barrieren je
   Fenster, KMS-Commit) und dort senken; jede Änderung mit neuem Messwert belegen.
c) **Späte Bilder „display“** im Lasttest (479 von 542, fertig 0,1–0,7 ms vor dem vblank,
   `lead 4.17`): hängt an b). Klären, wie viel Vorlauf nouveau braucht (`nv50_disp`,
   KWins Sicherheitsrand) und ob `MOST_LEARNT` (`schedule.rs`) reicht.
d) **Beim Einblenden genau alle 100 ms ein Bild „GPU“ spät** (0.5.103, fertig 0,15–0,34 ms nach
   Beginn des vblank): eigener Takt, Ursache offen.
Danach die Kosmetik nach den Entscheidungen des Besitzers, dann Schritt 4 (e)–(g) nach ADR 0028.

**Auf Hardware ungeprüft:** „frame done“ erst für gezeichnete Commits und fifo/commit-timing mit NVK (25.10.; Pacing von Terminal, Beize UI und Lasttest), Bildschirm weg und wieder da mit NVK (neuer Vulkan-Allokator je Bildschirm), Neutraining bei `link-status` BAD, Prompt beim Ziehen (4b75cb0, OSC 133 in `/usr/lib/beize/profile`,
`prompt.rs`), Damage-Clips, POLLOUT im Terminal, Rahmen echter Qt-/Chromium-Apps. Fehler, die nur
echte GPUs zeigen, sind in der VM nicht nachstellbar (dort shm, kein dmabuf): früh um Foto bitten.

**Entscheidungen des Besitzers, die nur hier stehen:** Menüleiste über Vollbild wirkt dunkel
satter, hell blasser (Material: Sättigung 1,8, Tönung 30/45 %): passt erstmal, nicht ungefragt
ändern. Fremde App-Symbole behalten ihre Form. Hintergrund „pixelig“: nie geprüft, offen, falls er
es wieder sieht. Apples Icons und „macOS-Look“-Sets sind tabu (VISION). Die feste Wartezeit auf
die Shell weicht einem Konzept (#40).

**Smoke-Aussetzer: Ursache belegt 25.10. (§9).** Die Hänger ganzer Gäste (fifo, Lasttest,
Minimieren rot in 4 von 12 Läufen) kamen vom WSL-Kernel ohne Preemption: Free Page Reporting
hielt eine CPU bis 141 ms (SCHED_FIFO-99-Wächter je CPU, SysRq-l: alle Backtraces
`page_reporting_process` → `hv_free_page_report`; ohne VMs 0 Lücken). Mit `preempt=full` 0 Lücken
über 50 ms, längste 42 ms, 4 von 4 vollen Läufen grün. `.wslconfig` gesetzt (wirkt ab dem nächsten
WSL-Start), `bz smoke` warnt sonst. Belegt sind die Lücken; dass sie die damaligen roten Läufe
waren, ist erschlossen (Muster passt, heute auch mit `none` 9 von 9 grün). Offen: sechs VMs (#41)
mit `preempt=full` erneut messen.
**Beobachten, Ursache offen:** Leerlauf-Ratsche: Compositor einmal in 3 s aufgewacht (1 von 6 Smoke-Läufen 25.10., Log überschrieben; nicht zugeordnet), RCU-Stall im Gast beim parallelen Smoke (bis 21 s, belegt 25.10.,
Issue; erschlossen: der Weckruf an die vCPU kam nicht, verschachteltes KVM), „libinput: system too slow“ (1 von ~8 Läufen),
„Boot0002 … Not Found“ im Update-Szenario (einmal).

**Offen in Schritt 4:** Die Shell liest Namen aus Desktop-Dateien nur einmal je App; fremde Apps
ohne Unit-Vorlage startet das Dock nicht (nur `Exec=`); Direct Scanout; Titelleiste im Vollbild
mit der Menüleiste. Die Beize-UI-Referenz entsteht offscreen in der VM: `./bz debug --machine run
'mkdir -p /run/t && mount -o ro /dev/disk/by-id/virtio-beize-test /run/t' '/run/t/beize-ui-test
--offscreen /run/s.ppm 800x600' 'gzip -9c /run/s.ppm | base64 -w0'`, auf dem Host zurück und nach
PNG.

**Upstream, wartet auf den Besitzer** (Entwürfe in `~/upstream`, `main` bleibt ohne fremde
Patches, der Messzweig wendet `kernel/measure/*.patch` an): nouveau EDID (240 Hz, gesendet 21.10.,
b4-Tag `sent/20261021-nouveau-edid-segment-…`), nouveau GSP-Interrupts (`IRQDEST`, gesendet
23.10.); Smithay: drei Berichte in `~/upstream/smithay/ISSUES.md`; ntpd-rs #2539 (am 23.10.
kommentiert, Backoff-Patch erst nach ihrer Antwort, keine Umgehung in Beize, ADR 0011).

**Grundlegende Vorschläge** (je ein ADR, noch nicht entschieden): Delta-Updates vor Updates übers
Netz (heute lädt jedes Update das ganze Image, 185 MB; Stücke des unkomprimierten Inhalts statt
Binär-Diff, setzt #5 voraus; zuerst das Delta zweier Bauten messen). Apps als signierte
EROFS-Images mit verity auf der Vertrauenskette von `/usr` (vor M4). Tests: von ~11 s je
VM-Start sind ~5,4 s der Gast, 1–2 s TPM-Emulation (#39), ~3 s unzugeordnet – messen, dann
Szenarien ohne Startprüfung aus einem Snapshot der laufenden VM.

**M3-Plan** (Besitzer, 17.10.): `m3-plan` sind #14 BTF (Basis), #17 Hardware (NVIDIA, Bluetooth
und Ton im Desktop; WLAN und zweite SSD in der Basis), #34 zram/oomd und #35 nftables (beide
Basis). #36 fwupd kommt mit M4. „(Basis)“ startet bis M4 mit dem Recovery-Key (ADR 0026, #38).

**Offen, kein Hindernis für M3**
- #5 bitgleiche Bauten: CI und lokal gleich bis auf `usr/lib/libdevmapper.so.1.02` (LVM2).
  Nächster Schritt: die Datei als CI-Artefakt, abschnittsweise vergleichen. Vergleichsbau unter
  `~/scratch/`: `git worktree add --detach …`, Quellen aus `build/cache`, `./bz build &&
  ci/digest.sh`.
- #4 vmspawn bewerten, #22 Hardware-Warnungen, #11 PCIe-Retraining mit Kernel 7.3 prüfen, #10
  beobachten, #12/#13 Upstream (sendet der Besitzer); #39 Firmware-Zeit der VM (TPM-Emulation).
- ShellCheck in die CI (Paket im Buildroot, mit der nächsten Lock-Änderung bündeln).
- **Platz (WSL und Linux):** `bz build` räumt selbst auf (`space.rs`): Quellen, die kein Rezept nennt,
  ältere Releases je Variante, `target/` bei neuer Rust-Toolchain oder über 10 GB (nicht bz selbst).
  Der Höchststand bleibt so begrenzt; die vhdx wächst nur bis dorthin. **vhdx verkleinern** (schrumpft
  nie von selbst; Sparse-VHD lehnt WSL ab, `--allow-unsafe` nicht verwenden): in WSL `sudo
  fstrim -av`, `wsl --shutdown`, dann `diskpart` als Administrator: `select vdisk
  file="C:\Users\Joe\AppData\Local\wsl\{4cae8202-1c42-41f8-840d-f8b846346de8}\ext4.vhdx"`,
  `attach vdisk readonly`, `compact vdisk`, `detach vdisk`.

**WSL-Home:** `~/beize`, `~/hw-logs` (Journale und Ergebnisse der Hardware-Tests),
`~/upstream` (Kernel-Bäume und Entwürfe – behalten), `~/scratch` (leer halten; Wegwerf-Arbeit,
am Ende des Schritts löschen).
