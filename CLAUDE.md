# Beize – Übergabe für Claude Code

Einstieg für jede neue Session; ersetzt den Chatverlauf. Stand: 2026-10-10, nach dem
Frühjahrsputz. Was im Code, in den ADRs oder im README steht, steht hier nicht noch einmal.

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
- Der Besitzer leert den Chat zwischen größeren Schritten: vorher §12 aktuell halten.
- Hardware des Besitzers: i7-13700K (Raptor Lake, x86-64-v3, kein AVX-512), ASUS ROG
  STRIX Z790-F GAMING WIFI (BIOS 3201; Intel 2.5GbE, Wi-Fi 6E + BT, Realtek-USB-Audio),
  RTX 3070 Ti, 32 GB RAM, Monitor 1440p @ 240 Hz.

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
| `tools/bz/src/` | Build-Werkzeug: `source` `recipe` `packages` `install` `kernel` `layout` `tree` `usr` `initrd` `secureboot` `image` `installer` `stamp` (Fingerabdrücke der Schritte) `util` (`run`, `say!`/`capture`); `vm/`: `machine` `console` (`Session`) `report` `checks` `smoke` `run` `debug` |
| `recipes/*.toml` | ein Rezept pro Paket: Quelle (gepinnt + signiert), `deps`, `stage`, `[build] script` |
| `keys/` | Maintainer-Schlüssel + `README.md`; `microsoft/` (gepinnt); `secureboot/` öffentliche Produktionsschlüssel + Anleitung für den Besitzer |
| `images/base.toml` | Pakete für `/usr` und für die initrd |
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
- Start in der VM: 5,3 s Gast-Uptime, erster Start ~8 s.
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
Netzwerk · 0012 Zeit (NTS). Neue Architekturentscheidung = neues ADR; spätere Änderungen
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
- Build-Jobs nach RAM begrenzt (1,5 GiB/Job), VMs nach CPUs/RAM (4 CPUs, 3 GiB) –
  nicht zurückdrehen.
- Werkzeuge, die zur systemd-Version im Image passen müssen (ukify, systemd-measure),
  kommen aus unserem systemd-Build (ADR 0009).
- Host-Werkzeuge, die `bz` aufruft, gehören in `ci/packages.txt`. Im Gast gibt es kein
  `sed`/`awk`/`grep` (Skripte in reinem Bash).
- Autotools-Abfragen beim Host (`AC_CHECK_PROG`) im Rezept beantworten (`ac_cv_…=yes`).
- Skripte unter `system/` müssen ausführbar sein (Test prüft das).
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
- Compiler-Flags generisch (`-O2 -pipe`); Ziel x86-64-v3 + Raptor-Lake-Tuning, Kernel zuschneiden.
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

**Beize läuft auf dem Rechner des Besitzers** (Samsung SSD 860 EVO, `/dev/sde` unter WSL,
Disk 0 in Windows; SanDisk 480 GB (E:, Daten) und Corsair (Windows) nie anfassen).
Neu installieren: `keys/secureboot/README.md` (`bz build`, `bz sign`, `wsl --mount`,
`bz install /dev/sde --guid "{6265697a-6500-4000-8000-000000000001}" --reinstall`).

**Frühjahrsputz erledigt** (Commits a3333f8…): knappe Ausgaben + Zusammenfassung;
`qemu.rs` → `vm/`; `bz debug` ersetzt `~/probe`; Smoke-Szenarien parallel auf Kopien der
Installation (217 s → 75 s, `--quick` 30 s); Stamps für Kernel/usr/initrd/image (No-op
36 s → 0,1 s); drei Tests waren zu nachsichtig und sind korrigiert (Secure-Boot-aus-Test lief
auf fremdem TPM; `systemunlock=refused` bei jedem „TPM2“-Fehler; fehlgeschlagene initrd-Units
bei stummer Konsole unsichtbar – jetzt `bootfailed=` aus dem Journal), je mit Gegenprobe;
ADR 0009/0010 als Endfassung.

**Nächste Aufgaben, in dieser Reihenfolge (vom Besitzer festgelegt):**
1. **Startzeit auf echter Hardware.** Normaler Start gemessen: Firmware 24 s + Loader 1 s +
   **Kernel 11,7 s** + initrd 4 s + System 1,6 s = 43 s (VM: Kernel 0,6 s, initrd 2,4 s).
   Hauptbaustelle Kernel; Verdacht (unbelegt): `console=ttyS0` auf echter Hardware oder
   Treiber, die auf Hardware warten – Kernel-Log mit Zeitstempeln ansehen. Firmware ist
   BIOS-Sache (Fast Boot). Danach initrd und Kernel verschlanken.
   **Keine Administratorrechte:** Besitzer ist in `wheel`, aber weder sudo noch su – root
   nur über die Notfall-Konsole. Entscheidung + ADR: sudo-rs oder run0 (braucht polkit).
   **Tastatur:** noch US-Layout – `kbd` fehlt, beize-setup muss nach dem Layout fragen
   (`/etc/vconsole.conf`). `ping` fehlt (iputils). Auf Hardware gut: Anmelden, Zeitzone,
   NTS-Zeit. Windows einmalig auf UTC stellen (`keys/secureboot/README.md` Schritt 4).
2. **Updates über die A/B-Slots** (systemd-sysupdate) – damit ist M2 fertig; auch nötig,
   damit Zertifikatsliste und tzdata nicht veralten.
