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
Netzwerk · 0012 Zeit (NTS) · 0013 Was läuft (Units im Image) · 0014 Toolchain x86-64-v3 + Härtung · 0015 Kernel: Konfiguration, Module, Microcode, Firmware · 0016 initrd aus Funktionen, Notfall-Shell, hwdb · 0017 Tastatur (XKB, kbd-Keymaps, Keymap der initrd in EFI-Variable) · 0018 Rechte (run0 + polkit, kein setuid, Wiederherstellung) · 0019 Konten (Beizes Kontodienst, `/etc/userdb`, Rechte durch Beweis; homed verworfen) · 0020 Updates (beize-updated + systemd-sysupdate, ML-DSA-signierte Liste, 4-GiB-Slots) · 0021 Buildroot (gepinnt, abgeschottet, lokal = CI) · 0022 SBOM und Schwachstellen (NVD, RustSec) · 0023 Absturzberichte (MiniDebugInfo, elfutils) · 0024 Bootloader-Konfiguration mit jeder Version · 0025 Varianten `base`/`desktop` (Vorgabe `desktop`, Start ohne Oberfläche) · 0026 TPM-Schlüssel für „(Basis)“ (mit M4) · 0027 Grafik (Smithay, nur Vulkan, eigene Bildschleife, Beize UI, NVK, Inter) · 0028 Shell (eigener Prozess, Menüleiste + Dock, `beize-shell-v1`, Strg-Modell, Phosphor) · 0029 Bildschirme (Hotplug, link-status, GPU und Bildschirme getrennt, aus bei Inaktivität). Neue Architekturentscheidung = neues ADR; spätere Änderungen
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

## 11. STAND UND NÄCHSTE AUFGABE (2026-10-28 spät: Shell (e) nachgebessert, Messbau 0.5.117)

**JETZT: Messbau 0.5.117 auswerten, sobald der Besitzer getestet hat.** Release fertig und
schon auf dem Stick (28.10. 22:25: `D:\beize-0.5.117`, SHA256SUMS geprüft OK; `D:\beize-test`
mit Skript + Testprogrammen); Branch + Tag vom Besitzer gepusht (1121851). Also gleich bei
„Ergebnisse“ einsteigen. Tag
`measure/desktop-0.5.117` (`measure/desktop` = 1121851, ein Commit auf `main` 4d3870d; gebaut,
Smoke grün 173 s). Stick-Dateien (`hw-test.sh` aus dem Tag + Testprogramme) liegen in
`~/scratch/stick`. Ablauf wie immer: **Besitzer** macht das Release (docs/hardware.md A) und
Force-Push von `measure/desktop` + Tag, sagt „Release fertig“ → Claude kopiert das Release
nach `D:\beize-0.5.117` und `~/scratch/stick/*` nach `D:\beize-test` (Prüfsumme der .efi
prüfen) → Besitzer testet → Claude kopiert den neuesten `D:\beize-test\results-*` (nach
20261028-185707) nach `~/hw-logs/` und wertet aus. Neu in Teil 2/6: d) Pille rund mit Luft zu
Globus/Uhr, Kacheln kleiner und bündig mit der Uhr; e) Fenster an der Titelleiste ziehen,
während Beize-Menü bzw. Kontrollzentrum offen ist (Menü zu, Fenster wandert sofort mit);
f) Kontrollzentrum mit Esc zu, sofort Klick aufs Beize-Zeichen (muss beim ersten Klick
aufgehen). Prüfen: Fotos, `Beize menu may:` = „yes“ dreimal, Zählung open/closed, Warnungen,
Bildtakt wie 0.5.116. CI des Pushs 4d3870d bei Übergabe noch offen: `gh run list -L 2` prüfen
(der Lauf davor, 471279c, war rot: `cargo deny` – behoben in ee35195).

**0.5.116 ausgewertet 28.10. abends (`~/hw-logs/results-20261028-185707`).** Alles bestanden:
logind „yes“ dreimal, Hell/Dunkel ~60× geschaltet (`appearance … chosen`), keine Warnungen;
Client-Test 4796/4800 (max 9 ms), Lasttest 114 912/115 200, rotiert 86 269/86 400, syncobj
30/30, FIFO 14 doppelte vblanks (bekannt, nouveau). Befunde des Besitzers am Test, alle behoben
in ee35195..4d3870d (ADR 0028 §3/§5 haben die Maße):
- **Klick neben offenes Menü auf eine Titelleiste zog nicht (belegt, Fehler):** Smithays
  `PopupPointerGrab::button` schließt und reicht den Klick weiter, aber ohne Klick-Grab →
  Compositor-Titelleiste sah ihn nie, `grab_for` lehnte das `move` einer App ab (`has=false`,
  per Debug-Log belegt). Jetzt `State::press_ends_popup_grab` (shell.rs): Menü zu und Zeiger los
  vor der Auswertung des Klicks (Besitzer wählte „Klick wirkt sofort“, wie anvil/niri).
- **Nach Esc ging der nächste Klick daneben (belegt, älter):** Smithays Grabs lassen erst beim
  nächsten Ereignis los und verwerfen diese Bewegung → Klick an der alten Zeigerstelle.
  `popup_grab_ended` lässt jetzt sofort los (`State::let_go_of`). Smoke fand es.
- **Schattenstrich neben 75-px-Kacheln:** Band „Fenster ohne Ecken“ wurde −1 px hoch (37,5 → 38
  aufgerundet), eine Schattenzeile doppelt (frame.rs `draw_shadow`, jetzt `max(0)`).
- **Maße (Besitzer):** Kontrollzentrum 6,5 % kleiner (`SIZE`, Kachel 75), rechts bündig mit dem
  Ende der Uhr, Leiste 20 px Rand; Pille rund, 28 hoch, unter dem Kontrollzentrum 54 breit; der
  Knopf ist seine Pille, Uhr/Globus 8 px davon (Apple ~6 pt, gemessen); Globus 20 px (Kugel
  ~16); Titel „Ethernet“ Gewicht 540, 2 px Zeilenabstand; Hell/Dunkel-Symbol 30 px.
- Smoke `menus`: Ziehen an der Terminal-Titelleiste bei offenem Menü (Gegenprobe ohne Fix rot).
- `deny.toml`: beize-shell darf beize-varlink (Netz über networkd, kam mit (e)).

**Danach mit dem Besitzer (offen aus (e)):** weiße Schrift auf dem Glas des Kontrollzentrums
und ob das Glas dafür angepasst wird (sein Wunsch, „noch nicht jetzt“); Bestätigungsdialog für
Neustart/Ausschalten; Abstand Globus↔Pille wirkt ~1–2 px größer als Pille↔Uhr (Kugel füllt ihr
Feld nicht ganz; angleichen, wenn er will); höheres Kontrollzentrum-Symbol **nicht** nötig
(Besitzer). Dann (f).

**Shell (e) gebaut 28.10.** (ADR 0028 §3, §5, §6 haben die Einzelheiten und Maße). Entwürfe
vorher abgenommen (Python-Mock-ups, `beize-design-values`-Notiz): Logo Variante 2, Menü ruhig
(Menü-Glas 85 %), Kontrollzentrum ohne Panel, Netz-Symbole `globe-simple(-x)`, dunkle Schrift.
Maße gemessen: Menüs an den HIG-Bildern (2 px/pt), Kontrollzentrum an Apples Pressebild.
- Compositor: xdg_popup-Grabs (wie anvil/niri; abgelehnte Grabs schließen das Popup; danach
  Tastatur zurück ans aktive Fenster, `popup_grab_ended`), Popups der Panels platziert, getroffen
  und mit Glas und Schatten gezeichnet; Glas mit mehreren Formen, Material `menu`, Schatten;
  `set_appearance` (gemerkt in `~/.config/beize/appearance`).
- Beize UI: Popups (`App::popup`, `close`, `set_input`, `flush`), `menu` als Baustein.
- Shell: Logo, Beize-Menü (logind über den System-Bus, `beize_systemd::login`; Abmelden beendet
  `XDG_SESSION_ID`), Kontrollzentrum (Netz über Varlink `Describe` + inotify auf
  `/run/systemd/netif` – sd-network ist in libsystemd nicht öffentlich, der Host-`cargo check`
  linkt nicht und zeigte es nicht, erst `bz build`), Hell/Dunkel.
- bz: Smoke-Prüfung `menus`, `bz debug '@click X Y'` (Tablet, pixelgenau).
- **logind-Lücke (belegt):** polkit nimmt für die Shell die Display-Sitzung; logind wählt sie
  bei `SetType` nicht neu → war der Benutzer vorher anders angemeldet (Smoke: serielle Konsole),
  „challenge“. Mit Patch in Beizes systemd (vorübergehend) „yes“. Patch + Erklärung in
  `~/upstream/systemd-logind-display/` (gegen main; systemds KI-Regeln: Besitzer schreibt den
  PR-Text selbst). Smoke meldet genau diesen Fall als „known“, jeder andere schlägt fehl.
- Prüf-Agent am Ende: 11 Befunde (u. a. Abmelden traf die falsche Sitzung, durchsichtige Fläche
  schluckte Klicks, Klick auf Dock/Leiste schloss das Menü nicht, logind blockierte das Öffnen),
  alle behoben; Größe: echt +52 KiB Compositor, +118 KiB Shell (Rest der `sizes.toml`-Anhebung
  ist Drift unter der 3-%-Toleranz seit 26.10.), keine lohnende Einsparung.
- Smoke danach 3 von 4 grün; einmal „the still desktop woke“: der Compositor 1× in 3 s, während
  run0 im Prüffenster authentifizierte – Ursache nicht belegt, nicht wiederholt (2 Läufe grün).
  Kommt es wieder: `still_and_signed_out` mit dem Journal des Compositors in dem Fenster prüfen.

**Danach: Shell, ADR 0028 „Reihenfolge“ (f) → (g).** Der Bildtakt ist auf
Beizes Seite fertig (0.5.115 unten); offen ist dort nur Treiberseitiges (nouveau-Bericht,
NVK-Stau – Besitzer-Entscheidung, kein Compositor-Code). Wünsche des Besitzers (28.10.):
Sperrbildschirm, Skripte anlegen (Frage: geht das schon? – im Terminal ja, mit `nano` im
Image; per Maus nein, es gibt noch keinen Dateimanager und keinen grafischen Editor), Dateien
auf dem Schreibtisch,
Kontrollzentrum mit Netz und Bluetooth, Rechtsklick-Menüs. Einordnung laut ADR 0028/VISION:
- (e) ✓ 28.10. (oben). Bluetooth/Ton/Medien erst mit PipeWire/BlueZ (#17, M4): „leere Kacheln
  gibt es nicht“ (§5) – Vorschlag an den Besitzer: #17 bald, sonst wirkt das Kontrollzentrum leer.
- (f) App-Menüs (`beize-menu-v1`), Alt+Tab, Snap; Rechtsklick-/Kontextmenüs gehören dazu.
- (g) Federanimationen.
- Eigenes ADR je: Sperrbildschirm (Super+L, §7; VISION: 1Password-Sperre daran), grafische
  Anmeldung, Suche, Benachrichtigungen. Sperren im Beize-Menü (e) braucht den Sperrbildschirm.
- Dateien auf dem Schreibtisch, Skripte per Doppelklick: kommen mit dem Dateimanager „Sift“
  (eigene App, zugleich Datei-Dialog, VISION) – noch kein ADR. macOS legt Dateien auf den
  Schreibtisch über den Finder, nicht über die Shell.
Offen zur Reihenfolge (Besitzer entscheidet): Sperrbildschirm mit eigenem ADR (Sicherheitsgrenze:
ext-session-lock-v1 wie KWin/sway, Anmeldung über PAM) vor oder nach (f). Arbeitsweise, die der
Besitzer bestätigt hat (28.10.): Werte erst messen, Entwürfe als Mock-ups abnehmen lassen, dann
bauen.

**0.5.115 getestet 28.10. (`~/hw-logs/results-20261028-154745`) – D bestätigt.** Client-Test
2 späte von 7163 in 30 s, **0 „drawing“** (0.5.114: 10, davon 7 „drawing“); Lasttest 3 spät,
114 976/115 200; rotiert **0 spät**, 86 286/86 400; syncobj 30/30, timed 239/239; keine
Warnungen. Wiederholte Flip-Ereignisse weiter da (Client-Test 25 in 30 s), alle echte (Art a),
keine Cursor-Bursts mehr gezählt; FIFO „7 pairs … 0, then 2“ = alle 7 Abweichungen. Rest der
späten Bilder: NVK-Stau („stalled“ 2,3–2,7 ms), zwei erste Bilder beim Start, eines nach einem
späten davor. Vorlauf weiter ein ganzes Bild, Latenz 7,5 ms Median (NVK-Stau, siehe unten).

**0.5.114 getestet 28.10. (`~/hw-logs/results-20261028-114239`).** syncobj 30/30 nach Beginn
des vblank (A auf Hardware bestätigt); Lasttest 114 912/115 200, rotiert 86 270/86 400; timed
239/239. **B senkte den Vorlauf nicht** (weiter ein ganzes Bild, Latenz 7,5 ms Median): die
Abweichung ~1 ms aus dem NVK-Stau (alle ~100 ms ~2,4 ms „stalled“) ergibt nach KWins Formel
2 + 2 × 1 + 0,46 + 1 > 4,17 ms – meine Vorhersage 3,46 ms hatte die Abweichung übersehen; KWin
gäbe hier dasselbe. Späte Bilder „display“ im Lasttest = derselbe Stau. **Befunde 2/4 belegt**
(Quelltext `dispnv50/disp.c` `nv50_disp_atomic_commit_tail`: Ereignis nach dem Notifier, Zähler
per `drm_crtc_accurate_vblank_count` aus der Zeilenposition): (a) echtes wiederholtes Ereignis
am Beginn des nächsten vblank mit Nummer/Stempel des vorigen, 10–29 je 30 s; jedes der 6
geloggten späten „drawing“-Bilder 4 ms danach, = FIFO-Paare „0 dann 2“ (12). Diese Bilder kamen
im normalen Takt, nur `due` war mit der alten Lage einen vblank zu früh. (b) Cursor-Züge allein:
nouveau meldet sofort (kein neues Bild → kein Notifier, `disp.c:2465`, `wndw.c:124`), mehrere je
Bild – kein Fehler, zählte fälschlich mit. KWin/Mutter/Weston geben den Stempel des Kernels
ungeändert an Clients (geprüft im Quelltext) → Client-Zeiten bleiben, Fix gehört in nouveau.
**D erledigt:** 74f07a6 (Ziel nie vor dem frühesten vblank des letzten Commits + 1, wie KWins
`scheduleRepaint`; Cursor-Ereignisse nicht gezählt; Tests + 4 Gegenproben rot; Prüf-Agent fand
D1 – zuerst `target` statt frühestem vblank gespeichert, hätte nach Pausen das zweite Bild einen
vblank gekostet – und D2 – ohne Phase –, beide behoben, Nachprüfung ohne Befund), Testclient
zählt „0, then 2“-Paare eigens (074b9f0). Smoke 3 grün + 1 rot (#41, Flip 21,6 s, VM).
**Messbau 0.5.115** (Tag `measure/desktop-0.5.115`). Erwartet: späte „drawing“-Bilder im
Client-Test ~0, „Flip-Ereignisse mit dem vblank davor“ ohne Cursor-Bursts (~10–30 je 30 s),
FIFO-Zeile „pairs … 0, then 2“ ≈ deren Zahl; Vorlauf/Latenz unverändert (NVK-Stau). Offen
(Besitzer): nouveau-Bericht upstream (wiederholtes Ereignis; NVK-Stau alle 100 ms).

**0.5.113 getestet 28.10. (`~/hw-logs/results-20261028-091933`).** Explizite Sync wirkt: späte
Bilder im Lasttest 109 → 5, „stalled“ Median 2,13 → 0,44 ms, 0 späte Bilder warteten auf
Clients; Lasttest 114 995/115 200, rotiert 86 308/86 400; busy 14 %; Resize 0–2 ms; keine
Warnungen; Kernel-Journal kam an (run0-Fix). Testfenster „bleiben stehen“ = fertig gezeichnet,
warten auf `--seconds` (Skripttext ergänzt). Vier Befunde, zwei Agenten + eigene Prüfung:
1. syncobj „30 of 30 before their vblank“: **kein Compositor-Fehler (belegt)** – Freigabe am
   Flip-Ereignis (Beginn vblank, GPU fertig), Präsentationszeit = Ende vblank (`drm_vblank.c`
   808-813), 0,461 ms bei 179 Leerzeilen. Test prüft jetzt gegen den Beginn (82e7311,
   vblank-Länge aus GETCRTC; Gegenprobe ohne Holds rot).
3. Vorlauf fast immer ein ganzes Bild, Latenz 6,6 → 7,5 ms: **belegt** – 2-ms-Boden lag auf der
   Erwartung statt (KWin `renderloop.cpp:176-177`) auf jedem Messwert; Gelerntes lernte aus
   „ready“ statt (KWin `DrmCommitThread::submit`) aus dem Commit. Beides wie KWin (7f1aa1d, Tests
   + Gegenproben rot; `pacing:` nennt Erwartung, Streuung, Gelerntes). Erwartet ~3,46 ms Vorlauf.
2./4. FIFO „0 dann 2“ (17×) und späte „drawn at once“-Bilder: **im Quelltext möglich, auf
   Hardware nicht belegt** – nouveau hat keinen Hardware-Zähler, das Flip-Ereignis am Beginn
   des vblank kann Nummer und Stempel des vorigen tragen (`drm_vblank.c:334-352`,
   `dispnv50/disp.c:2383-2395`); häufiger, weil wegen 3 fast alles am Flip-Ereignis gezeichnet
   wurde. 7f1aa1d zählt/loggt solche Ereignisse („flip event repeated the vblank before“).
   **D (nach 0.5.114, wenn belegt):** Einordnung nach KWin (Ziel nie vor wartendem Flip +
   1 vblank, `renderloop.cpp:71,106`), FIFO-Test „alte Nummer gemeldet“ getrennt; nouveau-Fix
   upstream = Besitzer-Entscheidung.
Prüf-Agent am Ende: kein Fehler; Kleinigkeiten behoben (`retrained` setzt die Zählung zurück).
Smoke: 4 Läufe, 3 grün, 1 rot im Resize-Check (Fenster 64×632 = Mindestbreite: Parken nach
(0,0) kam vor dem Loslassen an, libinput-Debounce 280 ms zu spät, VM-Stall; nur aus dem Log
erschlossen, = offener Punkt „libinput: system too slow“ unten). **Messbau 0.5.114** (Tag
`measure/desktop-0.5.114`, `measure/desktop` = 2affdd4 auf `main` 7f1aa1d, Smoke grün 162 s;
Stick-Dateien in `~/scratch/stick`). **Beim Besitzer:** Release, Force-Push `measure/desktop`,
„Release fertig“ → Claude kopiert, dann Test. Auswerten: Vorlauf/Latenz, „Flip-Ereignisse mit
dem vblank davor“, FIFO, späte Bilder; dann D.

**28.10.: Messbau 0.5.113 gebaut** (Tag `measure/desktop-0.5.113`, `measure/desktop` = 6b9ddb2,
ein Commit auf `main` 9c5ee5c; Smoke grün 155 s). `hw-test.sh` jetzt 5 Teile: Teil 3 zusätzlich
`beize-testclient --syncobj` mit nouveaus Timelines (`syncobj.log`, schwarzes Fenster); Kernel-
Journal schreibt `run0 bash -c 'journalctl "$1" … > "$0"'` selbst (Muster in WSL ohne run0
geprüft); Auswertung: späte Bilder, davon „auf Clients gewartet“ (soll 0), „stalled“ Median/Max,
`held:` im Lasttest. Teil „Bildschirm aus“ entfernt (0.5.111 belegt). **Beim Besitzer:** Release
(docs/hardware.md A), „Release fertig“ → Claude kopiert `D:\beize-0.5.113` und `D:\beize-test`
(Skript aus dem Tag, Testprogramme aus `build/sysroot/usr/lib/beize/test/` des Messbaus); danach
auswerten (Punkt 2 unten). Force-Push `measure/desktop` macht der Besitzer.

**28.10. nachts: Punkt 1 „Explizite Synchronisation“ umgesetzt** (ADR 0027 §7, `syncobj.rs`,
Smoke `syncobj`, Testclient `--syncobj`; Plan und Ergebnis je von einem Agenten gegengeprüft,
dessen Funde behoben; drei volle Läufe grün, Gegenproben rot: Acquire auf impliziten Fence,
ohne Holds, ohne eigene Protokollprüfung). Belegt unterwegs: Smithays `DrmCompositor` hält
Client-Puffer NICHT bis zum Flip (nur Scanout-Ebenen, `drm/compositor/mod.rs:206ff`) – die
Annahme in 1(iv) unten war falsch, Beize hält sie selbst (`Holds`, frühestens bis zum vblank);
CPU-signalisierte Timeline-Punkte haben den Stub-Fence mit Zeit des Kernelstarts
(`dma_fence_get_stub`), der Test misst deshalb, wann der Client das Signal sieht. In der VM gab
es bisher KEINE dmabufs (lavapipe ohne udmabuf → keine Modifier): jetzt `CONFIG_UDMABUF=y` (Fedora,
Besitzer entschied), die VM prüft dmabuf-Wege. **Smithay-Fix:** importierte Timelines zerstörte
Smithay nie (je Swapchain-Bild 2, bei jedem Resize) → `Drop` für `DrmTimelineDeviceSpecific`;
Fork `ScripterCarft/smithay`, Zweig `beize-0.7.0` (v0.7.0 + nur der Fix, Commit dfd93618) per
`[patch.crates-io]`, `deny.toml` erlaubt genau diese Git-URL, SBOM nennt die Git-Quelle. **Offen,
Besitzer:** PR an Smithay aus Zweig `syncobj-timeline-drop` (master + Fix, bbd88267, lokal
`~/upstream/smithay/src`); mit dem nächsten Smithay-Release fällt der `[patch]` weg. Nächster
Schritt: Messbau 0.5.113 (Punkt 2 unten; NVK-Apps müssen „synchronised explicitly“ im `held:`
zeigen, „waited for … client buffers“ 0). Bemerkt, nicht geändert: ein Client, der denselben
Puffer endlos neu anhängt, sammelt Release-Punkte (wlroots ebenso).

**AUFTRAG DES BESITZERS (27.10.)** – vor der Shell (Schritt 4 (e)–(g)), die „auf einem stabilen
Fundament bauen muss, nicht nur oberflächlich okay“:
1. ~~Leerlauf-Ratsche~~ **erledigt 27.10. abends** (unten). Offen aus 1: „close button … did
   not end the client“, „libinput: system too slow“, RCU-Stall #41, neu: Testclient `--timed`
   einmal 10 von 29 pünktlich, bis +91,7 ms (1 von 8 Läufen 27.10.; GPU-Zeiten der VM zur
   selben Zeit 200–265 ms; Journal der Sitzung nur bis 16 s erhalten → nicht belegt).
2. ~~CPU-/GPU-Fragen~~ **geklärt 27.10. abends, teils behoben; Rest misst der Messbau** (unten).
3. **Smoke-Zeit** (152 s) prüfen: was Zeit kostet, ob Fehler dahinterstecken (Wartezeiten,
   Neustarts), und senken, ohne Prüfungen zu verlieren.
4. **Gründliche Fehlersuche** im ganzen Desktop-Stapel (Compositor, Beize UI, Shell,
   Terminal, Session; Zeile für Zeile, nicht nur nach Mustern – unten „Zeile-für-Zeile-
   Durchsicht“), dazu die offene „told“-Sperre in `prompt.rs`. Regeln wie immer: Mechanismus
   belegen, Vorbild nennen, Smoke-Prüfung + Gegenprobe, drei volle Läufe; Befunde direkt beheben,
   größere Umbauten vorher dem Besitzer vorlegen.

**27.10. abends erledigt** (je Test/Smoke-Prüfung mit Gegenprobe, die rot wurde):
- **Leerlauf-Ratsche, Ursache belegt:** der Compositor beobachtete ganz `/etc` (inotify
  CLOSE_WRITE|MOVED_TO) für die Tastaturbelegung; jede dort zum Schreiben geöffnete und
  geschlossene Datei weckte ihn – systemd nimmt `/etc/.pwd.lock` für jeden Dienst mit
  `DynamicUser=` (ntpd-rs u. a.; ntp im Journal-Fenster war Zufall der Uhrzeit, ~86 s). In der
  VM: `: >> /etc/.pwd.lock`, `systemd-run -p DynamicUser=yes true`, `systemctl restart
  ntpd-rs` je genau 1 Weckvorgang des Haupt-Threads; Uhrzeitsprung, Bus-Signal, Units,
  Sitzungen 0. Fix: die Datei selbst beobachten wie systemd `/etc/localtime`
  (`manager_setup_timezone_change`), `/etc` nur solange sie fehlt (CREATE|MOVED_TO); Belegung
  nur anwenden, wenn sie sich unterscheidet (localed: ein Schreiben = zwei Ereignisse, O_TMPFILE
  verlinkt, dann geschlossen; localed vergleicht selbst). Smoke: das Leerlauf-Fenster startet
  einen `DynamicUser`-Dienst (Gegenprobe alte Überwachung rot, 1 Weckvorgang); neu
  `layout_follows` (zwei Belegungen nacheinander, je genau einmal; Gegenprobe rot).
- **Shell:** Uhr gesetzt ohne Minutenwechsel zeichnet die Menüleiste nicht neu (VM: jedes
  `date -s` ließ Shell und Compositor zeichnen).
- **Abblenden** zeichnet nur an den 255 Stufen eines 8-Bit-Bildschirms (Timer zur nächsten
  Stufe) statt 2400 ganzer Bilder bei 240 Hz, neun von zehn pixelgleich. Der späte erste
  Abblend-Frame (1,5 ms GPU) war kein Einzelfall: 30 von 2370 Abblend-Bildern spät, alle auf
  dem 100-ms-Raster unten; GPU nach 10 min heruntergetaktet (erschlossen), 1,5 ms je ganzes
  Bild in den ersten ~1,7 s.
- **Zeitplan:** ein Bild mit Vorlauf = Bildperiode wartete vom Flip-Ereignis (Beginn des
  vblank) bis zum Stempel (Ende), 0,43 ms je Bild verschenkt; 346 der 380 späten Bilder aus 7
  Hardware-Läufen hatten diesen Vorlauf. Jetzt am Ereignis, Plan und Lernen unverändert.
- **CPU:** Log-Filter mit `max_level_hint` (wie EnvFilter bei anvil/niri): abgeschaltete
  debug!/trace! kosten einen Vergleich statt des Filters (~30 ns je Aufruf gemessen).
- **Messpunkte:** Teil `damage` jetzt nur Smithays Tracker, neu `quads` (unser Quad-Bau je
  Element) und `planes` (Zeiger-Plane, Damage-Clips-Blob); späte Bilder nennen die GPU-Spanne
  auf CLOCK_MONOTONIC („on the GPU x ms, from a to b“, `VK_EXT_calibrated_timestamps`; in der VM
  geprüft).
- **Beize UI:** GPU-Bildplätze je Fenster (wie GTKs Renderer je Surface) statt zwei für alle;
  Fenster k wartete auf die GPU-Arbeit von Fenster k−2. **Nutzen unbelegt:** in der VM gleich
  (303→164 statt 302→162, dort begrenzt die CPU) – der Messbau zeigt es (0.5.111: 9380→9061),
  sonst zurücknehmen.
**Geklärt, nicht geändert** (zwei Helfer, belegt aus 7 Hardware-Läufen und Quelltext):
- **Späte Bilder:** etwa alle 100 ms (10-Hz-Raster in jedem Lauf) ~2 ms Stau zwischen Commit
  und Fence bei ~0,4 ms GPU-Arbeit; kein 100-ms-Timer im Compositor oder in nouveaus Host-Code.
  Erschlossen: periodische Arbeit der GSP-Firmware (DVFS?). nouveau wartet im Worker auf der CPU
  auf die Fences und braucht ~0,8 ms vor Beginn des vblank (kein Bild, das vor −0,79 ms fertig
  war, kam zu spät). Die GPU-Spanne im Messbau zerlegt den Stau. 0 späte Bilder trotz Stau ginge
  nur mit einem zweiten Bild unterwegs bei vollem Vorlauf (KWin 6.1, Mutter 48 „dynamic triple
  buffering“) – widerspricht ADR 0027 §6 „höchstens eins wartet“: **27.10. nachts entschieden: nein** (unten).
- **Fenster 2–12:** der Lasttest zeichnet sie in fester Reihenfolge; der Verlust über Fenster 1
  wächst mit der Position (0, −1, 2, 5, 10, 17, 38, 73, 126, 210, 319): spätere Commits
  verpassen das Bild. Fenster 1 verliert nur, was der Compositor spät ist.
- **CPU je Bild:** Smithays Tracker ~75 µs auf schnellem Kern (Nachbau der Lastszene), ~60
  Pipeline-Wechsel (blend/opak) je Bild. Kandidat nach dem Messbau: eine Pipeline mit
  Blend-Schalter (`VK_EXT_extended_dynamic_state3`) oder immer vormultipliziert.
  `implicit_waits` bleibt (26.10. bewusst als Sicherheitsnetz behalten).
**27.10. nachts: FIFO-Wackler geklärt – KEIN Compositor-Fehler; GRUND der tagelangen VM-Fehler.**
Ein Agent (Code-Durchsicht) und Claude unabhängig, gleiches Ergebnis. Kette vblank → `lift`
(`state.rs` `present`) → `blocker_cleared` → Commit → `changed` → `schedule_render` ist richtig:
`flipping` ist vorher gelöscht, der Zeichen-Timer wird immer gesetzt, das Bild sofort gezeichnet.
Die Lücken: in der VM zeichnet **lavapipe auf den vCPUs des Gasts**, der Haupt-Thread wartet darauf
(`sync` ≈ GPU-Zeit: 411,87 ↔ 408,90 ms; lavapipes sync_file-Export wartet, erschlossen); unter
Last braucht ein Bild > 16,7 ms → einen vblank später. Belegt in 4/4 roten Läufen: Vorlauf gekappt
(`lead … max 16.68`), GPU bis 590 ms, Client 0 Lücken. `pacing` zählt sie nicht, weil virtio den
Zeitstempel vor das Ereignis setzt (Ziel = übernächster vblank; auf Hardware stimmt die Zählung,
der Agent-Vorschlag `target` zu ändern hätte sie dort verfälscht → verworfen). Dazu: virtio-gpu
hat einen **Software-vblank** (hrtimer, `DRM_CRTC_VBLANK_TIMER_FUNCS`); ein verspäteter Tick zählt
die Sequenz um 2 hoch. Und #41: auch im **grünen** Lauf 27.10. nachts stand eine leerlaufende vCPU 21 s
(`rcu_preempt … t=21008 jiffies`; verlorener Weckruf bei KVM in Hyper-V erschlossen, nicht belegt).
**Gemeinsamer Grund:** Smoke-Prüfungen verlangten Echtzeit-Takt von einer VM ohne echte GPU, ohne
echten vblank, mit einfrierenden vCPUs, und wurden mit getunten Toleranzen beruhigt
(`MISSED_VBLANKS = 2`) – jede Lastspitze sah aus wie ein Compositor-Fehler. **Behoben:**
fifo/timed prüfen das Protokoll ausnahmslos (120/120, 0 verworfen, Sequenz streng steigend – war
vorher nicht geprüft –, 0 vor der Zeit) und vom Takt nur, was ein Compositor-Fehler mit ALLEN
Bildern machte (Barriere/Zeit einen vblank zu spät → kein einziges im Abstand eins). Den Takt misst
der Messbau auf Hardware. Regel ab jetzt: **in der VM keine Zeit-Toleranzen; Takt nur auf Hardware.**
- Smoke gibt bei rotem Desktop-Check das Sitzungs-Journal ins Konsolen-Log (`JournalOnFailure`;
  die liegengelassene Maschine behält davon nichts). `bz debug` überschreibt `logs/boot`!
- run0 verlor einmal die ganze Ausgabe (Exit 0): upstream-Mechanismus, `ptyfwd.c` `drained()`
  prüft nur `TIOCINQ`, Flip-Puffer des PTY nicht (systemd PR #43058 beschreibt es). Nicht
  nachgestellt; Bericht an systemd = Besitzer-Entscheidung.
- Zeitplan-Änderung ist in der VM praktisch wirkungslos (2 Treffer gemessen) → nicht die Ursache.
**Nicht committet** (alles im Arbeitsbaum, Smoke ohne Last 7/8 grün): siehe oben „27.10. abends“.
**27.10. nachts erledigt (Liste der Helfer-Agenten vom 27.10.):** (2) `pacing:` sagt „a whole refresh
for N frames“, late-Zeilen „stalled x“ (ready − committed − GPU); (3) Teile `begin`/`record`/`end`
statt `record`, „quads median …, in draws median …“ (VM-Lasttest: 271 Quads in 49 Draws);
(4) `beize-load-test --rotate` (Startfenster je Runde); Beize UI-Bildplätze geprüft (Atlas:
ein Bild, Barriere in derselben Queue → sicher; leben so lange wie die App, Fenster schließen
einzeln gibt es nicht). **Entschieden (Besitzer: nur mit Vorbild, das stabil ist):** kein zweites
Bild unterwegs – Weston, niri, sway haben eins; Mutter brauchte Jahre und Rückfälle; der 10-Hz-Stau
ist erst zu zerlegen (6), sonst wäre es ein Symptom-Pflaster. `told`-Sperre bleibt (Ereignis statt
kitty's Timer `resize_debounce_time`; Test belegt Bedarf; jede Ausgabe der Shell löst sie).
`beize-session` durchgesehen, ohne Befund. Smoke-Zeit 158 s: kritischer Pfad Desktop 133 s,
keine Wiederholungen dahinter; größter fester Teil ist das echte Abblenden (10 s, GNOME) –
bewusst echt geprüft.
**0.5.112 getestet 27.10. abends (`~/hw-logs/results-20261027-181253`).** Lasttest **114 346 von
115 200** (0.5.111: 111 761), Fenster 1–12 gleich (9531 → 9525; vorher 9380 → 9061): die Treppe
kam von den zwei GPU-Bildplätzen für alle Fenster einer App – Bildplätze je Fenster belegt,
bleiben. `--rotate` ebenso gleich. Testfenster 4790/4800, FIFO 2394/2399 (`1648:0` = nouveau
meldet die alte Sequenz, wenn die Scanout-Position noch vor dem vblank liest, `drm_vblank.c`;
kein übersprungenes Bild; Zeichnen am Flip-Ereignis sicher), timed 238/239 (ruckelt sichtbar mit
Absicht: alle 2,5 vblanks). Jedes späte Bild: Commit ~2,8 ms vor dem vblank, **die GPU beginnt
erst ~2,2 ms später**, arbeitet 0,1–0,4 ms, nouveau braucht ~0,8 ms → zu spät. Zwei Agenten
(27.10., Quelltext geprüft): (a) `nouveau_exec.c:252` trägt jede Submission als WRITE in **alle**
geteilten Puffer der VM ein; unser `implicit_waits` (EXPORT_SYNC_FILE READ) und Smithays
`DmabufBlocker` warten damit auf spätere Arbeit der Clients (Lasttest ~20× mehr späte Bilder je
geplantes Bild als ohne GPU-Clients); (b) Rest ~2,5 ms auf 100-ms-Raster auch ohne GPU-Clients =
Firmware/Strom (erschlossen); (c) **hohe GPU-Priorität gibt es auf NVK/nouveau nicht**
(`nvk_queue.c:403-406` lehnt > MEDIUM ab, Kernel ohne Prioritäten); (d) CPU ist kein Hebel
(0,2 ms je Bild, `record` 0,02 ms) – die frühere Pipeline-Idee entfällt. Kernel-Journal fehlte:
`run0` im Skript mit umgeleiteter Ausgabe → „Access denied“.
**27.10. nachts erledigt (Punkte 2–4 des Besitzers, je mit Vorbild, Smoke + Gegenprobe):**
(2) Vorlauf nach KWins `RenderJournal` (Quelltext gelesen: 0,5 s geglättet + 2× Abweichung, die
über 6 s abklingt) statt Maximum der letzten 120 Bilder (vergaß einen Stau nach 0,5 s); `learnt`
blieb, ist schon KWins `DrmCommitThread` (+voll, −1/10, ≤ min(3 ms, ½ Periode)). (3) Flip-Wächter
wie KWin (`s_pageflipTimeout` 1 s): Fehler „page flip timed out … kernel driver“, weiter warten,
später „arrived after all“; liegt das Ereignis ungelesen, hing der Compositor (info); Timer nur
solange ein Commit wartet (Leerlauf-Check grün), Gegenprobe 1 ms rot. KWin setzt NICHTS zurück –
Agent 2s „zurücksetzen und neu zeichnen“ wäre eigene Erfindung gewesen. (4) late-Zeilen „waited
for N client buffers“, Summe „N waited for clients' later work“ (`sync::pending`). Prüf-Agent
fand und behoben: Frist zählt erst ab erfülltem GPU-Fence (sonst falscher „Treiberfehler“ bei
langer Client-GPU-Arbeit; KWin `isReadyFor`), Fence mit Fehler zählt als fertig; Timer nur vor
seiner Frist entfernen; nur das vblank des wartenden Bildschirms; Zählung je Puffer statt Plane.
Smoke-Wackler „still desktop woke“ (1 von 7): seit dem run0 im Leerlauf-Fenster reichte es über die
Minutengrenze (Uhr der Shell) – jetzt wird bei neuer Minute nach Ruhezeit neu gemessen.
**OFFEN, in dieser Reihenfolge (Besitzer: jeder Punkt in EINEM Schritt, nicht in a/b teilen;
am Ende jedes Schritts prüft ein Agent unabhängig gegen; Mechanismus belegen, Vorbild aus dem
Quelltext lesen, nicht aus dem Gedächtnis):**
1. ~~Explizite Synchronisation~~ **erledigt 28.10.** (oben; 1(iv) des Plans war falsch, s. dort).
2. Messbau 0.5.113 (**gebaut 28.10.**, oben; Test beim Besitzer) mit 1 + `hw-test.sh`: Kernel-Journal per `run0 bash -c 'journalctl … > "$0"'
   datei` (run0 bleibt am Terminal). Auswerten: „waited for … client buffers“ muss 0 sein,
   „stalled“, späte Bilder.
3. **Danach entscheiden (Punkt 5 des Besitzers):** bleibt der Firmware-Rest spürbar, ein zweites
   Bild unterwegs nach KWin/Mutter („dynamic triple buffering“; Apple: Metal-Standard 3 Bilder,
   Android SurfaceFlinger) mit ADR – erst mit diesen Zahlen, sonst wäre es Retusche.
4. Weitere Funde Agent 2 (nicht dringend): Beize UI sendet immer die ganze Fläche als Damage
   (`VK_KHR_incremental_present`, GTK); `frame_timer` (jetzt + Periode) statt auf den nächsten
   vblank (niri); `Frame::Deferred` im Normalbetrieb unerreichbar (bleibt als Schutz).
5. #41 (vCPU 21 s still) eigens untersuchen; run0-Ausgabeverlust kommt mit systemd v263.
6. Bewusst NICHT: `implicit_waits` für Clients ohne syncobj weglassen; Smithays Damage-Tracker
   ersetzen; GPU-Priorität (gibt es nicht); Pipelines zusammenlegen (record 0,02 ms).

**0.5.111 getestet (27.10., `~/hw-logs/results-20261027-143412`). Besitzer: „hat richtig gut
geklappt“, Bildschirm aus/an funktioniert.** Teil 6: Abblenden nach genau 600 s, aus 10,0 s
später, Maus nach 90 s: an, neuer Modus sofort, `dpms` On, Kernel schweigt (nouveau trainiert
den Link beim Wiedereinschalten ohne Fehler). Lasttest 111 761 Bilder (0.5.109: 110 768), „at
most 1 on their way“; Testfenster 4763 von 4800 (wie 0.5.109), 35 spät; Resize 3 ms, `held:`
flach (8–16 dmabufs); Warnung nur der bekannte Smithay-Blob beim Wiedereinschalten. Ein spätes
Bild „GPU“ beim ersten Abblendbild (ganzer Schirm, 1,5 ms GPU) – einmalig, hinnehmbar.

**27.10.: idle-inhibit + Bildschirm aus erledigt** (ADR 0029 §7, `idle.rs`; Smoke
`screen_goes_off` in einer zweiten Sitzung mit 3 s Frist, Gegenproben rot: Inhibitor
ignoriert, Eingabe ohne Wirkung, CRTC nicht aus („kernel says On“), aus mit Bildtakt des
Bildschirms (180 statt ≤ 5 Bilder in 3 s); drei volle Läufe grün). 10 min ohne Eingabe → 10 s
Abblenden (GNOME) → `DrmSurface::clear` (niri); aus = für Clients „weg“ (`Gpu::on`). macOS'
10 min sind aus dem Gedächtnis, nicht belegt (Suche fand keine Apple-Quelle); GNOME 5 min.
Smoke gesamt ~140 → 152 s (zweite Anmeldung + 10 s Abblenden im Desktop-Szenario). Unterwegs
behoben: aus meldete Clients noch den Bildtakt (FIFO-Client 60/s statt 1/s).
**Auf Hardware offen (nächster Messbau):** kommt der Monitor nach dem Abschalten wieder
(nouveau trainiert den Link neu; Monitor aus/an blieb auch unter Fedora schwarz – anderer Weg,
aber dieselbe Schwachstelle möglich)? Teil für `hw-test.sh`: am Ende 10 min + 10 s nichts
anfassen, dann Maus; Skript liest `the screen is off`/`input: the screen is on again` und
`dpms`. Ohne eigenen Messbau-Schalter (Frist kommt aus der Umgebung beim Start).
**0.5.110 ausgewertet (27.10., `~/hw-logs/results-20261027-131921`):** nur der Screenshot-Teil
lief (Lasttest/Client/UI-Logs leer); Besitzer: Screenshot gut – Wellenkanten 1 px, gehaltener
Knopf ohne hellen Rand. **Offen:** lange Zeile + Pfeil hoch nicht berichtet; ob die
„told“-Sperre (`beize-terminal/src/prompt.rs`) noch nötig ist, weiter unbelegt.
`measure/desktop` und Tags vom Besitzer gepusht (origin = 73314bd); `measure/desktop-old` löscht der
Besitzer (`git branch -D measure/desktop-old`, Claudes Rechteprüfung verweigert es).

**0.5.109 getestet (26.10., `~/hw-logs/results-20261026-222434`).** Lasttest 110 768 Bilder
(0.5.108: 111 749), Fenster 1–12 9306 → 8921 (Auftrag (3) unten); Pacing 30 s: 204 von 6966
spät (display 180, GPU 24); Testfenster 4763 von 4800; Resize 1 und 4 ms, `held:` flach; keine
Warnungen. Foto fehlte (Skript suchte erst ab Teil 6, Besitzer drückte in Teil 5; `/run/user`
mit dem Neustart weg) → Skript kopiert alle Fotos der Sitzung (`measure/desktop` 0edc1b8, auf
dem Stick). Fotos 27.10. (`~/hw-logs/results-20261027-085037`) ergaben, alle auf main, Smoke grün:
- **Wellenkanten weich** („pixelig“ war das): 0,004 Bildschirmhöhen = 5,8 px. Jetzt 1 px,
  Abdeckung nach dem Abstand quer zur Kante wie `quad.frag` (2bf5391, Besitzer einverstanden).
  Beleg: Python-Nachbau des Shaders trifft das Foto auf ±1, neu 1 Zeile, Licht der Kante bleibt.
- **Gedrückter Knopf mit hellem Rand** (e0443e um bf4942) → ganzer Kreis dunkelt (55a581d, Test
  + Gegenprobe).
- **Terminal: lange Zeile seitlich verschoben („<…“), Reste beim Blättern:** bash hing nicht an
  ncurses, nahm sein eigenes termcap (`/etc/termcap`, gibt es nicht) → Readline ohne Umbruch und
  ohne Löschen bis Zeilenende. Fix fa905e1 (ncurses als Abhängigkeit, `bash_cv_termcap_lib`);
  bash des Sysroots im PTY: vorher `<`, jetzt Umbruch; ohne ncurses scheitert der Link (geprüft).
  **Offen, auf Hardware prüfen:** ob die „told“-Sperre in `beize-terminal/src/prompt.rs` (eine
  Größe je Zeichnung der Shell) noch nötig ist. Die Prompt-Leerung selbst bleibt (Tests nutzen
  ein bash mit ncurses; kitty/Ghostty machen es ebenso). Besitzer nannte die Sperre „unsauber“.
Diese drei Fixes sind im Messbau 0.5.110 (oben); Besitzer prüft Wellen, Knopf, lange Zeilen +
Pfeil hoch, Resize.

**E erledigt (26.10.):**
- (a) Knöpfe als SDF (c7035c4): Kreise (`Scene::circle`), Striche und Dreiecke rechnet der
  Quad-Shader je Pixel (`STROKE`, `ROUND`, `TRIANGLE`); entfernt: 9 SVG-Konstanten,
  `Scene::shape`, `Glyphs::shape`. Zoom bleibt zwei Dreiecke (Besitzer). resvg zeichnete den
  Rand ungleichmäßig, die Terminal-Referenz änderte sich nur an den Kreiskanten (Besitzer sah
  den Vergleich). Smoke: Zeichen unter dem Zeiger, Gegenprobe rot, 3 Läufe grün.
- SVG raus aus dem Compositor (f11ffcf): Symbole und App-Symbole in `beize-draw` hinter dem
  Feature `icons` (`glyphs/icons.rs`), nur Beize UI schaltet es ein. Compositor 7569 → 6517 KiB,
  0 resvg-Zeichenketten (vorher 71). CI prüft den Compositor auch allein. Features nur
  hinzufügend: `Atlases::colour` bleibt ohne Feature.
- (c) Titeltext von der Shell **verworfen** (Besitzer einverstanden): bleibt im Compositor wie
  Weston (`xwayland/window-manager.c`, cairo/pango); unser Text-Stapel ~1,3 MB < Westons ~3 MB
  (pango, harfbuzz, freetype, fontconfig, fribidi, gemessen). ADR 0028 §2.
- (b) Symbole als Phosphor-Schrift: **nicht jetzt** (bringt nach f11ffcf keine Größe; resvg bleibt
  für App-Symbole in der Shell). Erst wenn Menüs/Kontrollzentrum viele Symbole neben Text zeigen.
- Gemessen, Rest des Compositors: ~3 MB nicht aufgeschlüsselt – bei Gelegenheit `cargo bloat`
  je Crate (Besitzer fragte; fest eingebaut statt geteilt ist für Beize richtig: ganzes
  `/usr`-Abbild je Update, nur der Compositor nutzt Smithay; Teilen erst prüfen, wenn viele
  Beize-Apps laufen).

**0.5.108 getestet (26.10., `~/hw-logs/results-20261026-205934`).** Lasttest 111 749 Bilder
(vorher 90 025), alle 12 Fenster bei 240/s; Testclient beim Resize < 64 MB statt 11,2 GB (systemd
262.1 schreibt `Consumed … memory peak` nur über 1 s CPU bzw. 64 MB, `unit.c` `MENTIONWORTHY_*`);
Resize 0–1 ms, `held:` flach. Besitzer: alles gut, Lasttest flüssig. Späte Bilder wieder 214 von
9390 bei vollen 240/s (0.5.107 bei ~165/s: 12), fast alle „display“ – Auftrag unten.
Achtung: `bz smoke` baut nicht – nach einem Messbau erst `./bz build` (sonst prüft er das alte
Abbild, Fehler „missing: /usr A“).

**Auftrag (Besitzer, 26.10.: „sauber weiterarbeiten, nicht wieder Symptome bearbeiten“):**
1. ~~0.5.107 und 0.5.108 auswerten~~ erledigt (oben).
2. **Weicht etwas ab: erst den Mechanismus belegen, dann ändern.** Keine Messzeile, kein Timer,
   kein Sonderfall „damit es geht“. Fragen in dieser Reihenfolge: Welche Annahme ist falsch? Was
   machen Weston/Mutter/KWin/niri/GTK genau dort (Quelltext lesen, Besitzer mag Weston als
   Vorbild)? Lässt sich dafür Code entfernen? Jede Änderung mit Smoke-Prüfung **und** Gegenprobe,
   die rot wird. Ein Messgerüst nur befristet und im selben Schritt wieder entfernen.
3. ~~CI-Lauf 40304913614~~ erledigt: RUSTSEC-2026-0383 (webpki-root-certs, nur im Lock von
   ntpd-rs, nicht gelinkt) → `[cargo.update]` 1.0.10; `bz` hebt jetzt die kompatible von mehreren
   Versionen an (f8c6ecf). `main` gepusht 26.10. Offen beim Besitzer: Force-Push `measure/desktop`
   und Tags. Nächster Messbau 0.5.108 (Lasttest-Erwartung oben).
4. ~~E~~ erledigt (oben). ~~idle-inhibit + Bildschirm aus~~ erledigt 27.10. (oben). D mit der Shell.
   nouveau-Patch für das still scheiternde Link-Training
   (Befund 2, unten) schreiben – senden tut der Besitzer.
**Auftrag des Besitzers (26.10.) für einen Chat nach der Shell, am besten nach idle:** (1) warum
die späten Bilder (Lasttest 0.5.108: 214, je eines kostet jedem Fenster ein Bild – Fenster 1 hat
9380 von 9600); (2) warum der Compositor 28–31 % je Bild braucht (`damage` 0,37, `record` 0,21 ms
Median) – wie Glas 0,77 → 0,00 ms, den Rest ebenso angehen; (3) warum Fenster 2–12 stufenweise
weniger Bilder haben (9380 → 9055; Vermutung: der Lasttest zeichnet nacheinander, späte Commits
verpassen das Bild – unbelegt).
**Offen:** späte Bilder „display“ (anderer Chat); ~~Leerlauf-Ratsche~~ (27.10. behoben, oben);
seltener Smoke-Fehler
„close button of the compositor's frame did not end the client“ (1 von ~20 Läufen 26.10., nach
systemd 262.1; in 4 Folgeläufen nicht wieder, Logs überschrieben) – beim nächsten Auftreten Logs
sichern (`build/work/logs/boot`).

**0.5.106 getestet (26.10., `~/hw-logs/results-20261026-123053`). Besitzer: „Fixes haben so gut
wie nichts gebracht“ – den gemeinsamen Grund suchen, was bei uns merkwürdig ist; lieber alten
Code entfernen und einmal sauber lösen als mehr Code („Jungle-Pflanzen weg, eine schöne holen –
aber erst prüfen“).** Befunde:
- **Bildschirm:** Monitor zeigt „Kein DP Signal von Ihrem Gerät“, auch auf Strg+Alt+F2 (die
  Konsole des **Kernels**). Compositor: Modus-Commit + vblank, `connector Connected, link good`,
  danach keine Commits mehr (auch nach dem VT-Wechsel: Modus, ein Bild, Stille); Kernel-Journal
  schweigt. Spricht für nouveau/GSP-Anzeigeweg, nicht belegt.
- **Resize:** stürzt nicht mehr ab, zieht aber nach dem Loslassen bis zu 5 s weiter; der
  Compositor misst ab dem *verarbeiteten* Loslassen 1–14 ms (`resized …`) → die 5 s liegen davor
  (Eingaben verspätet verarbeitet?) oder im Inhalt des Terminals. Pfeil-hoch im Terminal fühlt
  sich verzögert an. `held:` flach (9–13 dmabufs, 55–65 offene Dateien).
- **Späte Bilder:** Lasttest 186 von 6976 spät, fast alle „display“; Testfenster 2 von 4805.
**Plan (nächster Chat, Besitzer stimmt zu):** (1) Besitzer: Konsolentest **vor** `beize-session`
– Monitor aus/an, Kabel ab/an: kommt die Konsole zurück? Nein → Kernel/Treiber (upstream), ja →
unser Wiederkommen-Pfad. (2) Referenz auf derselben Hardware: Fedora-Live mit GNOME (nouveau):
Monitor aus/an, Gleichmaß. (3) Messbau: Alter jeder Eingabe beim Verarbeiten (libinput-Zeit vs.
jetzt), wartende Commits je Surface, im Terminal Configure→Bild. Dann erst entscheiden, was
rausfliegt; Kandidaten des Ungewöhnlichen: eigene Bildschleife (`output.rs`/`schedule.rs`) statt
Smithays `DrmCompositor` (niri), fifo/commit-timing (seit 0.5.105 stauen sich Commits), Beize UIs
neue Swapchain + `device_wait_idle` je Größe; nouveau-GSP-Anzeige als seltenster Teil.
**(1) erledigt 26.10.: auch OHNE Sitzung (Konsole, kein Compositor) kein Bild nach Monitor
aus/an** → Befund 2 liegt unter dem Compositor (Kernel/Treiber/unsere Kernel-Konfiguration oder
-Patches). **(2) erledigt 26.10.: Fedora-Live (GNOME, nouveau, gleiche HW) hat denselben Fehler
(kein Bild nach Monitor aus/an); Resize dort einwandfrei, Zeiger bleibt Resize bis zum Loslassen**
→ Befund 2 ist upstream (nouveau/GSP oder Monitor), nicht Beize; Resize-Nachziehen ist **unseres**.
Quelltext (v7.3-rc6, gleich in 7.2): `nv50_sor_atomic_enable` ignoriert die Rückgabe von
`nouveau_dp_train()`; `r535_dp_train_target` macht aus `ctrl->err` bzw. EAGAIN ohne `retryTimeMs`
still einen Fehler (ctrl.c loggt EAGAIN/EBUSY nicht, Rest nur `NV_DEBUG`); nouveau setzt die
Eigenschaft `link-status` nie → unser „link good“ sagt nichts; der Kopf läuft weiter, vblanks
kommen. **Erschlossen, nicht belegt:** Link-Training nach dem Aufwachen scheitert still (warum
auch spätere Modesets/VT-Wechsel scheitern, offen: veraltete DPCD-Fähigkeiten aus dem Aufwachen?).
Gleiches Muster in Berichten: nouveau GP108 (Arch 2020, DPMS aus/an half), NVIDIA-Treiber mit
1440p/240-Hz-Samsung (NVIDIA-Forum 346806). Nächster Schritt (Besitzer): Fedora-Live mit
`drm.debug=0x106 nouveau.debug=disp=debug log_buf_len=16M`, Kernel-Log auf den Stick; dazu
Monitormodell, gilt es bei 60 Hz, hilft Kabel ab/an, Windows ok? Dann Patch an nouveau (Fehler
melden, `link-status` BAD wie i915 → Compositor modesettet neu). Wechsel zum NVIDIA-Treiber
besprochen: nicht als erster Schritt (geschlossener Userspace statt NVK, ADR 0027).
**Prüfung 26.10. (Besitzer: alles Grundfalsche finden und belegen; 4 Prüfer + Nachprüfung). Plan
vorgelegt, NICHTS geändert – auf Freigabe warten.** Kern (belegt): Beize UI zeichnet bei jedem Anlass
sofort (`window.rs` `draw_once`, Terminal je PTY-Lesen/Taste), nur Configures warten auf den
Frame-Callback; FIFO mit 4 Bildern. Mesa 26.2.5 drosselt FIFO **nur ohne** wp_fifo über den
Frame-Callback (wsi_common_wayland.c ~3424); seit unserem wp_fifo (4ad22e3, 25.10.) blockiert
Present nicht mehr, Mesas Kommentar nennt 4 Bilder „deeply problematic due to excessive latency“.
Unsere fifo-Umsetzung hat Beize UI also unbemerkt ungedrosselt – der Resize-Rückfall kam mit
0.5.105, dem ersten Bau mit wp_fifo; `window.rs:4-5` behauptet das Gegenteil. Neue Swapchain je
Größe hebt die Bildgrenze auf. Pfeil-hoch-Verzögerung damit erklärt; die 5 s Nachziehen: Mechanismus
belegt, Größe nicht (flache `held:` spricht gegen eine tiefe Schlange). Weitere Befunde (belegt):
`resized … ms` hängt am Ack, nicht am angewandten Commit (Smithay setzt current = last_acked im
Post-Commit-Hook); Configure je Mausereignis (~1000/s, Mutter/KWin: eins offen); dmabuf doppelt
importiert (`check_dmabuf`); jeder Flip-/Zeichenfehler beendet Compositor und Apps; Vorlauf bei
240 Hz immer 4,17 ms (`schedule.rs`-Lernen wirkungslos); `each_surface` ohne `unmapped`;
Terminal-`told`-Sperre ohne Vorbild; Kernel: `RESET_ATTACK_MITIGATION=y` (setzt MOR bei jedem
Start, niemand löscht es; Fedora aus), `UBSAN_TRAP`, `PAGE_TABLE_CHECK_ENFORCED`, IOMMU an+strict
(Fedora aus/lazy), serielle Konsole auf Hardware, keine Diagnose (DEBUG_KERNEL/Lockup, WATCHDOG,
DRM_PANIC, DP_AUX_CHARDEV aus), Strg+Alt+Entf in der Sitzung ohne Wirkung (VT im K_OFF,
keyboard.c), Compositor ohne Echtzeit (`RestrictRealtime=yes`). Widerlegt: `nouveau.atomic=1`
(Upstream schaltet es ab 7.4 ein), 7 s Kernel durch init_on_free (gemessen: 5 s PCIe + 1,9 s).
Fedora-Vergleich gilt nur bis 144 Hz (ohne unseren EDID-Patch kein 240 Hz) und legacy KMS.
**Besitzer 26.10.: Plan ja, aber Kernel nicht blind wie Fedora (erst belegen, ob Problem),
Strg+Alt+Entf egal (Rauschen). Reihenfolge: 1. Takt.** ~~1. Takt~~ **erledigt (21b33a6)**: Beize
UI zeichnet nur, wenn das letzte Bild gezeigt ist (`App::ready`, nummerierte Frame-Callbacks),
MAILBOX wie GTK, kleinste Bildanzahl; Terminal/Shell/Lasttest zeichnen bei `ready`. `told`-Sperre
des Terminals bleibt (ohne sie belegt „beizbeize $“ im Prompt-Test). Smoke: Lasttest „at most 1 on
its way“, Gegenprobe altes Verhalten = 4, rot; 3 Läufe grün. Hardware offen (nächster Messbau).
**2. Compositor erledigt 26.10.** (je 3 Smoke-Läufe grün): Messgerüst 64142a6 zurückgenommen
(4d70e81); dmabuf einmal beim Anlegen importiert wie anvil/Weston, `held:` ohne offene Dateien
(8980ec7; LimitNOFILE bleibt); Bildfehler beendet die Sitzung nicht mehr (76ada72, ohne
Gegenprobe – Fehler in der VM nicht auslösbar); ungezeigte Fenster in `each_surface` (8133532,
Testclient `--timed` mit Zeit im ersten Commit, Gegenprobe rot). **Geprüft und bewusst gelassen:**
Configure je Mausbewegung (anvil macht es genauso, Beize UI nimmt nur das neueste); Resize-Ende
über `current_state` (anvil ebenso); `pending_writes` je Bild (Vulkan hat keine implizite
Synchronisation, Sicherheitsnetz, µs); `schedule.rs`-Lernen (Bildtakt/späte Bilder = anderer
Chat); `told`-Sperre (s. o.).
**3. Kernel geprüft 26.10.:** Hardware ohne serielle Konsole/Getty (5d4af80); Lockup-/Hung-Task-
Erkennung, DRM_PANIC, iTCO-Watchdog (76a0bee; DEBUG_KERNELs Eigenes aus). **Bleibt
bewusst** (Upstream-Härtungsvorlage, kein Problem belegt): UBSAN_TRAP+BOUNDS (Trap statt stiller
Speicherverletzung), PAGE_TABLE_CHECK, IOMMU an+strict (DMA-Schutz; Kosten unbelegt, erst
messen), EFI_DISABLE_PCI_DMA, INIT_ON_FREE (Besitzer). ITMT/P-Kerne aktiv (ACPI_CPPC_LIB).
**Offen, Entscheidung Besitzer:** `RESET_ATTACK_MITIGATION` – Kconfig: nur sinnvoll, wenn das
System MOR beim sauberen Herunterfahren löscht; Beize tut das nicht → Firmware löscht den RAM
vermutlich bei jedem Start (Kosten unbekannt). Prüfen auf dem PC: `od -An -tx1
/sys/firmware/efi/efivars/MemoryOverwriteRequestControl-e20939be-32d4-41be-a150-897f85d49829`
(Datei fehlt → wirkungslos). Als Nächstes: Messbau für die Hardware.
**Auftrag des Besitzers an den nächsten Chat (26.10.):** suchen, bis etwas **grundlegend Falsches,
Systemkritisches** gefunden ist – die Probleme liegen „wohl woanders“; Beize darf dafür grundlegend
geändert werden. **Vor E** (SVG/Knöpfe) zu lösen. Weiteres Symptom: **Strg+Alt+Entf startet neu,
bleibt aber selten hängen**, zweites Drücken nötig. Ungewöhnlich bei uns und zu prüfen (neben den
obigen): eigene Kernel-Konfiguration (`kernel/beize.config`), neuester Stable-Kernel, eigene
nouveau-Patches des Messzweigs (`kernel/measure/*.patch`), nouveau mit GSP auf Ampere. Vergleich
mit Fedora (gleiche Hardware) zuerst auswerten; gleichen Kernel/Konfiguration gegenüberstellen.
**Nicht gepusht:** `main` (9 Commits seit fa629f0) und Tag `measure/desktop-0.5.106` – CI-Lauf
40304913614 scheiterte am Job „Known vulnerabilities“ (neue Meldung, nicht unsere Commits) – erst klären, dann pushen. Messzweig braucht Force-Push (Besitzer).

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
**Beobachten, Ursache offen:** RCU-Stall im Gast beim parallelen Smoke (bis 21 s, belegt 25.10.,
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
