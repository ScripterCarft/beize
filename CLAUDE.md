# Beize – Übergabe für Claude Code

Diese Datei ist der Einstieg für jede neue Session. Sie ersetzt den bisherigen
Chatverlauf. Stand: 2026-10-09, nach „Zeit mit NTS“ (ADR 0012).

## 1. Was Beize ist

Ein eigenes Betriebssystem auf dem Linux-Kernel: **so konsequent wie macOS, so
frei wie Windows, technisch so offen wie Linux.** Kernel, Treiber und
Basisbibliotheken kommen aus Upstream (selbst gebaut), alles darüber –
Compositor, Shell, Einstellungen, Dienste – wird eigener Code (Rust).
**Lies vor größeren Entscheidungen `docs/VISION.md`** (sieben Grundprinzipien,
Design-System, Performance-Budget, Nicht-Ziele, offene Entscheidungen).

Meilensteine: M1 Bootet in QEMU ✓ · **M2 Echte Hardware (in Arbeit)** ·
M3 Compositor · M4 Alltagstauglich · M5 Feinschliff. Detaillierter Stand: `README.md`.

## 2. Zusammenarbeit mit dem Besitzer

- Kommunikation **auf Deutsch**, verständlich erklären, ehrlich über Schwächen
  („nicht optimiert, bewusst noch nicht“ statt Schönfärberei).
- Anspruch: **„richtig machen statt pfuschen“** – professionell, skalierbar,
  getestet. Fehler an der Wurzel beheben und gegen Wiederkehr absichern (Test,
  Compile-Time-Check, Guard in `bz`).
- Commits und Pushes auf `main` sind freigegeben. Commit-Nachrichten englisch,
  aussagekräftiger Body, Abschluss `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  Ein neuer Signaturschlüssel kommt in einen **eigenen** Commit.
- **1Password ist tabu.** Claude greift nie auf den Tresor zu (kein `op`).
  Produktionsschlüssel erzeugt, verwahrt und benutzt der Besitzer selbst (ADR 0006).
- Hardware des Besitzers: i7-13700K (Raptor Lake, x86-64-v3, kein AVX-512),
  ASUS ROG Strix Z790-F Gaming WiFi (angenommen; Intel 2.5GbE, Wi-Fi 6E + BT,
  Realtek-USB-Audio), RTX 3070 Ti, 32 GB RAM, Monitor 1440p @ 240 Hz,
  freie zweite SSD für Tests.

## 3. Umgebung

- Host Windows 11, Werkbank **WSL2 `Ubuntu-26.04`**, Benutzer `joe`,
  Repo **`~/beize`** (Windows-Pfad `\\wsl.localhost\Ubuntu-26.04\home\joe\beize`).
  Nicht auf NTFS verschieben (Kernel-Builds, Rechte, Groß-/Kleinschreibung).
- GitHub: `git@github.com:ScripterCarft/beize.git`. Git in WSL nutzt per
  repo-lokalem `core.sshCommand` Windows-`ssh.exe` mit dem 1Password-SSH-Agent –
  **Push hängt, wenn 1Password gesperrt ist**: Besitzer bitten zu entsperren.
  `gh` ist in WSL installiert (Ubuntu-Paket); angemeldet wird es vom Besitzer selbst
  (`gh auth login`). Damit: `gh run list`, `gh run view --log-failed`, `gh run cancel`.
- Läuft Claude Code auf Windows (Arbeitsverzeichnis `F:\code-projects`), Befehle so ausführen:
  `WSL_UTF8=1 wsl.exe -d Ubuntu-26.04 -- bash -l -s <<'EOF' … EOF` (Heredoc!
  `bash -c '…'` wird umgeparst, `$?`/`$(…)` expandieren falsch; Git Bash
  wandelt `/tmp/…`-Argumente in Windows-Pfade).

## 4. Architektur (Stand jetzt)

```
Firmware (Secure Boot; PK/KEK/db von Beize + Microsoft im Profil „mit Windows“, ADR 0010;
          im Setup-Modus schreibt systemd-boot sie aus /loader/keys/auto/ selbst ein)
  └─ prüft systemd-boot (signiert, eigene SBAT „beize“)
       └─ prüft UKI beize_<ver>+3.efi  = Kernel + initrd + Kommandozeile
            + signierte PCR-11-Richtlinie (.pcrsig/.pcrpkey)          (alles signiert)
            Kommandozeile: usrhash=<root hash> mount.usr=/dev/mapper/usr …
            └─ initrd (systemd): beim 1. Start systemd-repart → Slot B + Root anlegen
               (Root = LUKS2, TPM-Slot an PCR 7 + signierte PCR-11-Richtlinie „initrd“;
               Recovery-Key wird hier angelegt), dm-verity für /usr,
               Root finden (gpt-auto) + per TPM entsperren, switch-root
                 └─ System: /usr (EROFS, dm-verity, read-only) + / (ext4 in LUKS2: /etc, /var, /home)
```

| Partition (GPT, UAPI.2-Typen) | Inhalt | Entsteht |
| --- | --- | --- |
| ESP 512 MiB (FAT32, 4-KiB-Cluster) | systemd-boot, UKIs, `loader/loader.conf`, `loader/keys/auto/*.auth` | Image |
| `/usr` A data 1 GiB + verity 64 MiB, Label `beize_<ver>` | System; UUIDs = Hälften des Root-Hashes | Image |
| `/usr` B data + verity, Label `_empty` | nächstes Update | 1. Start |
| Root (LUKS2 `Encrypt=tpm2`, `TPM2PCRs=7` + signierte PCR-11-Richtlinie mit Referenz `initrd`, darin ext4, `FactoryReset=yes`), wächst in den Rest | alles des Besitzers | 1. Start |

Nur `/usr` wird ausgeliefert; `/etc` wird beim 1. Start aus
`/usr/share/factory/etc` (tmpfiles) und von systemd erzeugt. Boot-Counting:
3 Versuche, `systemd-bless-boot` entfernt den Zähler. Alle Größen, Typen,
Versionen und die repart-Definitionen stehen **nur** in `tools/bz/src/layout.rs`.

## 5. Repository

| Pfad | Inhalt |
| --- | --- |
| `os/` | **Beizes eigener Code im Image** (Rust, Workspace-Mitglieder). Bisher `beize-tpm` (initrd: TPM-Neubindung nach Recovery, ADR 0010) |
| `tools/bz/src/` | Build-Werkzeug (Rust, ~3800 Zeilen): `main` CLI · `source` Download/Pin/Signatur · `recipe` Rezepte · `packages` Abhängigkeiten, Fingerprints, Build-Umgebung · `install` Sysroot, merged-/usr, Besitz-Manifeste · `kernel` · `layout` Disk-Layout · `tree` Dateibäume, strip · `usr` /usr-Image + dm-verity · `initrd` · `secureboot` Schlüssel (Test/Produktion, 1Password), Einschreib-Dateien, Signieren · `image` UKI + Disk · `qemu` `run`/`smoke` · `util` |
| `recipes/*.toml` | ein Rezept pro Paket (20): Quelle (gepinnt + signiert), `deps`, `stage` (toolchain/target), `[build] script` |
| `keys/` | öffentliche Maintainer-Schlüssel + `README.md` (Herkunft, Fingerprints, Ausnahmen); `keys/microsoft/` Microsofts Secure-Boot-Zertifikate + dbx (gepinnt); `keys/secureboot/` öffentliche Produktionsschlüssel + Anleitung für den Besitzer |
| `images/base.toml` | Pakete für `/usr` und für die initrd |
| `system/usr/` | eigene Dateien in `/usr` (Units, Boot-Report, Werkseinstellungen) |
| `system/initrd/` | eigene Dateien der initrd (repart-Drop-in, Recovery-Key, `beize-tpm.service`); dort gibt es bash, aber **keine coreutils** |
| `kernel/config/beize-base.config` | Kernel-Fragment, nach dem Merge verifiziert |
| `ci/packages.txt`, `ci/system.sh` | einzige Liste der Build-Abhängigkeiten; kompletter CI-Ablauf |
| `docs/adr/`, `docs/VISION.md` | Entscheidungen, Vision |

## 6. Bauen und Testen (in `~/beize`)

```sh
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
./bz verify      # alle Quellen: Pin + Maintainer-Signatur; listet unsignierte Ausnahmen
./bz build       # kernel → Pakete → usr (+verity) → initrd → image  (inkrementell)
./bz smoke       # DER Abnahmetest, muss nach jeder Änderung grün sein
./bz run         # interaktiv (Ctrl-A X), VM-Disk bleibt bis zum nächsten Image erhalten
./bz package <name> · ./bz usr · ./bz initrd · ./bz image · ./bz owner /usr/bin/bash
# nur der Besitzer, im Terminal (TTY-Sperre), mit 1Password – nie von Claude ausführen:
./bz secureboot new-keys · ./bz sign · ./bz install /dev/sdX --guid "{…}"
# Anleitung für echte Hardware: keys/secureboot/README.md
```

Logs: `build/work/logs/<paket>.log`. Voller Neubau ~10–15 min (GCC zweimal),
inkrementell Sekunden bis wenige Minuten. `./bz smoke` installiert das Image auf
eine frische 8-GiB-Disk (jede VM mit eigenem swtpm, Firmware im **Setup-Modus**: das
erste Einschalten lässt systemd-boot PK/KEK/db/dbx einschreiben, danach prüft `bz` die
Zertifikate in den Firmware-Variablen) und verlangt: Secure Boot an,
`usr=verity`, `root=encrypted`, `running`, **keine `[FAILED]`/`[DEPEND]`-Zeile in
initrd oder System**, sauberes Herunterfahren, 1. Start legt alle Partitionen an
(Typ + Label geprüft), Boot-Counting „gut“, 2. Start = gleiche Machine-ID;
Recovery-Key beim 1. Start angezeigt, beim 2. nicht mehr; Gegentests: dieselbe Disk
mit anderem TPM → Abfrage „Please enter recovery key for disk root“, der Test tippt
den Key ein, System startet; mit Secure Boot aus (Schlüssel bleiben) → Root bleibt zu; unsigniertes UKI → „Access denied“,
ein verändertes Byte in `/usr` → „data block … is corrupted“. Bei fehlgeschlagenen
Units gibt der Boot-Report deren Journal aus; jeder Start muss per DHCP (virtio-net,
QEMU-NAT) `routable` werden, DNS 10.0.2.3 lernen und `_gateway` über NSS auflösen (`BEIZE-FAILED-LOG …`). Zum Schluss der
Ablauf des Besitzers (`new-keys` → `sign`) mit Wegwerf-Schlüsselspeicher statt 1Password,
Ergebnis wird eingeschrieben und gebootet.
Aktuell: Start 5,23 s Gast-Uptime (Kernel 0,56 + initrd 2,42 + System 2,25 s),
erster Start ~7 s (Verschlüsseln + Formatieren). Ganzer Smoke-Test ~3 min.

CI (`.github/workflows/ci.yml`): fmt/clippy/test bei jedem Push; auf `main`,
manuell und wöchentlich `ci/system.sh` in privilegiertem `ubuntu:26.04`-Container
mit KVM (verify, build, smoke). Ein neuer Push bricht ältere Läufe ab
(`concurrency`). Gebaute Pakete, Toolchain, Sysroot, Stamps und der Kernel-Objektbaum
liegen im Actions-Cache (nach jedem Lauf gespeichert, auch nach Fehlschlag/Abbruch);
die Fingerprints von `bz` entscheiden, was neu gebaut wird. Der erste volle Bau auf
einem 4-Kern-Runner dauert Stunden (gcc-pass1 allein ~23 min) – `bz` meldet bei
langen Paketen jede Minute „still building …“. Bis 2026-10-08 griff der Cache nie
(zstd/gzip, siehe §10), und der erste vollständige Lauf scheiterte am Boot-Test
(lvm2-Regel, siehe §9). Beides behoben und per sauberem Container-Bau lokal
bestätigt; der erste grüne GitHub-Lauf steht noch aus (baut wegen `ENV_REVISION`
einmal alles neu, ~1 h).

## 7. Sicherheitsmodell

- **Quellen:** jede gepinnt (SHA-256 bzw. Commit) **und** von Upstream signiert,
  geprüft mit `gpgv` gegen `keys/<name>.gpg`. Ohne Signatur nur mit expliziter
  Begründung `unsigned = "…"` (einzige Ausnahme: json-c). systemd kommt aus dem
  signierten Git-Tag.
- **Build:** eigene Cross-Toolchain (`x86_64-beize-linux-gnu`), kein Host-Header im
  System; reproduzierbar (fester Zeitstempel `BUILD_EPOCH`, feste UUIDs, Salt aus Inhalt).
- **Boot:** Secure Boot → systemd-boot → UKI (Kommandozeile versiegelt, `editor no`)
  → `usrhash` → dm-verity für jeden `/usr`-Block.
- **Daten:** Root-Partition LUKS2 (aes-xts), TPM-Slot an PCR 7 (Secure-Boot-Zustand)
  **und** die signierte PCR-11-Richtlinie mit Referenz `initrd` – nur die initrd eines
  signierten Beize-UKI bekommt den TPM-Schlüssel, das laufende System nicht noch einmal
  (kein Schutz gegen ein kompromittiertes System: dm-crypt hält den Schlüssel im Kernel; Boot-Report
  `systemunlock=refused`, Header-Prüfung in `bz smoke`); kein Passwort-Slot.
  Recovery-Key wird beim 1. Start in der initrd erzeugt und einmal angezeigt
  (Konsole + `/run/beize/recovery-key`), nie auf Platte/Journal (ADR 0009 + Nachtrag).
  Weigert sich der TPM, öffnet der Recovery-Key; `beize-tpm` bindet nur neu, wenn der
  gemessene Zustand zum signierten Profil passt (Secure Boot an, PK/KEK/db, Autoritäten)
  und ein neuer TPM ausdrücklich bestätigt wurde (ADR 0010). `bz smoke` prüft: dbx-Update
  (neu gebunden), fremdes db-Zertifikat (abgelehnt), neuer TPM (no → abgelehnt, yes →
  gebunden), Secure Boot aus + Recovery-Key (abgelehnt).
- **Schlüssel (ADR 0010):** Beize ist Plattform-Eigentümer (eigener PK/KEK);
  Profil „Beize + Windows“ schreibt zusätzlich Microsofts KEK/db und dbx ein.
  Tests/CI: Wegwerf-Schlüssel `CN=Beize TEST ONLY …` (PK, KEK, db) + PCR-Schlüssel unter
  `build/work/secureboot-test/`. Produktion: private Teile nur im 1Password-Tresor
  „Beize“, öffentliche Teile + signierte `.auth`-Dateien in `keys/secureboot/`;
  `bz secureboot new-keys` (einmalig, PK/KEK/db/PCR) und `bz sign` (db + PCR) laufen nur
  im Terminal des Besitzers, Schlüssel nur in `/dev/shm`. **Der Besitzer hat die
  Produktionsschlüssel noch nicht erzeugt** (Stand 2026-10-08).

## 8. ADRs (`docs/adr/`)

0001 Linux-Kernel aus Upstream · 0002 glibc + systemd · 0003 unveränderliches
signiertes Image · 0004 Entwicklung in WSL2 · 0005 Cross-Toolchain, Rezepte,
signierte Quellen, Besitz-Manifeste · 0006 Secure Boot, Test- vs.
Produktionsschlüssel · 0007 nur `/usr`, A/B, Einrichtung beim ersten Start ·
0008 dm-verity für `/usr` · 0009 Root mit LUKS2 + TPM2 (PCR 7), signierte
PCR-11-Richtlinie im UKI, UKI-Werkzeuge aus eigenem systemd · 0010 Beize als
Plattform-Eigentümer, Vertrauensprofile, Produktionsschlüssel/1Password · 0011
Netzwerk (networkd + resolved, keine eingebauten Fremd-Server) · 0012 Zeit (ntpd-rs + NTS,
tzdata, Zertifikatsspeicher, RTC in UTC). Neue
Architekturentscheidung = neues ADR.

## 9. Regeln für Rezepte und Code

- Nur `/usr` ausliefern: `bz` bricht ab bei Dateien außerhalb von `/usr`; Altlasten
  im Rezept löschen (mit Kommentar) oder nach `/usr/share/factory` verlegen.
- **pkg-config liefert beim Cross-Build Pfade mit Sysroot-Präfix** (Unit-, udev-,
  tmpfiles-, sysusers-Verzeichnisse). Solche Verzeichnisse immer explizit setzen;
  `bz` bricht ab, wenn etwas unter `home/…/build/` installiert würde.
- `meson -Dauto_features=disabled`: jedes benötigte Feature ausdrücklich einschalten.
- Abhängigkeitsschleifen vermeiden (libdevmapper ohne udev-Sync, weil systemd → cryptsetup → lvm2).
- **Eigener Rust-Code im Image:** Crate unter `os/<name>` (Workspace-Mitglied, läuft also
  in `cargo clippy/test --workspace` mit), Rezept mit `repo = ["os/<name>", "Cargo.toml",
  "Cargo.lock"]` (Inhalt geht in den Fingerprint) und Skript `"$CARGO" fetch --locked` +
  `"$CARGO" build --offline --locked --release --target x86_64-unknown-linux-gnu` (Vorbild
  `recipes/beize-tpm.toml`). `bz` setzt `CARGO`/`RUSTC` (Toolchain aus
  rust-toolchain.toml), `CARGO_HOME=build/cache/cargo`, `RUSTFLAGS=-C linker=…-gcc`
  (nur Zielartefakte; Build-Skripte bleiben beim Host-cc – Crates mit Build-Skript
  meiden, solange es geht). Krypto nie selbst schreiben (RustCrypto `sha2`).
- Rezepte **ohne** `repo` behalten ihren Fingerprint (Test
  `recipes_without_repo_sources_keep_their_fingerprint`) – sonst baut alles neu.
- **Ein Paket sieht beim Bauen nur seine (transitiven) `deps` im Sysroot** – `bz`
  verschiebt alle anderen installierten Dateien währenddessen nach
  `build/sysroot-hidden/` und holt sie danach (oder beim nächsten Lauf) zurück.
  Fehlt einem Rezept dadurch etwas, gehört es in `deps` – nicht die Sperre
  umgehen. Hintergrund: lvm2 fand lokal (nach systemd neu gebaut) libudev und
  schrieb eine andere udev-Regel als im sauberen CI-Bau → `/dev/mapper/usr`
  wurde nie „ready“, CI-Boot rot.
- Build-Jobs werden nach RAM begrenzt (1,5 GiB/Job) – nicht zurückdrehen.
- **Werkzeuge, die zur systemd-Version im Image passen müssen** (ukify,
  systemd-measure), kommen aus unserem systemd-Build, nicht vom Host: ukify nach
  `usr/lib/beize-build/` (Build-only, nie ausgeliefert), Ziel-Binaries laufen über
  `sysroot/usr/lib/ld-linux-x86-64.so.2 --library-path …` (siehe `image.rs`).
- Host-Werkzeuge, die `bz` aufruft, gehören in `ci/packages.txt` – fehlende fallen
  sonst erst in der CI auf. Gegenstück: im Gast gibt es kein `sed`/`awk`/`grep`
  (Skripte in reinem Bash).
- Konfigurationsoptionen, die Autotools beim Host abfragen (`AC_CHECK_PROG`),
  im Rezept beantworten (`ac_cv_…=yes`), sonst hängt das Ergebnis vom Host ab.
- Skripte unter `system/usr` müssen ausführbar sein (Test prüft das). **Achtung:
  Write/Edit über den Windows-Pfad setzt den Modus auf 644** – solche Dateien in WSL
  ändern (z. B. Python-Patchskript) oder danach `chmod +x`.

## 10. Bekannte Fallstricke der Umgebung

- WSL-VM kann nach einem OOM instabil werden (Distro wird alle paar Minuten
  beendet) → `wsl --shutdown`. Ursache war GCC mit 24 Jobs + JetBrains-Backend
  (`remote-dev-serv`, >3 GB; `build/` in der IDE ausschließen).
- WSL beendet die Distro ohne angehängte `wsl.exe`-Sitzung → lange Jobs als
  Hintergrund-Befehl laufen lassen, der die Sitzung hält; Logs nach `~`, nicht `/tmp`.
- `qemu -serial stdio` liest stdin – in Skripten `< /dev/null`.
- **CI rot, lokal grün?** Den CI-Lauf lokal 1:1 nachstellen (sauberer Bau, ~20 min):
  Arbeitsstand ohne `build/` nach `~/ci-x` kopieren (`rsync -a --exclude /build
  --exclude /target ~/beize/ ~/ci-x/`), dann `sudo podman run --rm --privileged
  --device /dev/kvm -v ~/ci-x:/w -w /w public.ecr.aws/docker/library/ubuntu:26.04 ci/system.sh`
  (lange Läufe per `sudo systemd-run --unit=… -p StandardOutput=file:…`).
  Danach `build/work/initrd-tree` bzw. `usr-tree` mit `diff -rq` vergleichen.
- `actions/cache` im Container: Kompression (zstd/gzip) ist Teil des Cache-Schlüssels –
  `zstd` wird deshalb **vor** dem Wiederherstellen installiert (sonst nie ein Treffer).
- FAT32 braucht ≥ 65 525 Cluster (Compile-Time-Check in `layout.rs`); beim
  Installieren auf größere Disks Backup-GPT verschieben (`sfdisk --relocate gpt-bak-std`).
- systemd-repart lässt Partitionen ohne `SizeMaxBytes=` wachsen – auch bestehende
  (Test `only_root_grows`). Vorab prüfen: `systemd-repart --definitions=… --dry-run=yes <Kopie>`.
- systemd-Konsolenzeilen enthalten ANSI-Farbcodes **mitten im Text** („Failed to
  start \e[0;1;39mCryptography…“) → vor dem Vergleich `strip_ansi`.
- Im Gast nachsehen (PCRs, Journal …): VM per Skript mit stdin-Pipe starten und
  Befehle an die Autologin-Shell auf ttyS0 schicken (QEMU-Argumente wie `qemu.rs`,
  swtpm vorher starten). So wurde z. B. der PCR-11-Wert mit `systemd-measure
  calculate` abgeglichen.

## 11. Offene Punkte (außer der aktuellen Aufgabe)

- Notfall-Shell geht nicht (`sulogin` fehlt; braucht libxcrypt → mit Benutzerpasswörtern).
- initrd 15,9 MiB / 2,3 s – enthält noch ganz systemd; verschlanken. hwdb nicht generiert.
- Compiler-Flags generisch (`-O2 -pipe`); Ziel x86-64-v3 + Raptor-Lake-Tuning, Kernel auf Hardware zuschneiden.
- Updater (systemd-sysupdate), Verhalten bei
  verity-Fehlern zur Laufzeit (Neustart → Rückfall?), WLAN, NVIDIA, Bluetooth,
  Start von der zweiten SSD, Installer.
- TPM: Rollback – jedes ältere, mit demselben PCR-Schlüssel signierte UKI erfüllt die
  Richtlinie ebenfalls (Abhilfe erst mit Versions-Sperre/Schlüsselwechsel).
  `CONFIG_TCG_TPM2_HMAC` ist aus (ADR 0009). pcrlock verworfen (ADR 0010).
- Debug-Hilfe außerhalb des Repos: `~/probe/probe.py NAME KEY|"" TPM-DIR CMD…` bootet
  eine liegengebliebene Test-Disk aus `build/work`, tippt ggf. den Recovery-Key und
  führt Befehle auf der Autologin-Shell aus (kein grep im Gast: Bash-`case`).
- Profil „Nur Beize“ (ADR 0010): vorher klären, welche Option-ROMs der Rechner
  braucht (RTX 3070 Ti: GOP mit Microsoft UEFI CA 2011 signiert) – Hashes aus dem
  TPM-Ereignisprotokoll statt der ganzen CA. Profilwechsel aus dem laufenden System.
- Recovery-Key wird bisher nur auf der Konsole gezeigt – das Onboarding (M3+) muss
  ihn grafisch anzeigen und bestätigen lassen.
- CI, vom Besitzer zurückgestellt („im Hinterkopf“): `permissions: contents: read`,
  `push` nur auf `main` + `pull_request`, Dependabot für Actions (+ Actions auf
  Commit-SHAs pinnen), rustup-Warnung „$HOME differs“ im Container. Repo ist privat
  → Actions-Minuten sind begrenzt, volle Läufe sparsam.

## 12. STAND UND NÄCHSTE AUFGABE (2026-10-09)

**Beize läuft auf dem Rechner des Besitzers** (Samsung SSD 860 EVO, `/dev/sde` unter WSL):
Produktionsschlüssel (1Password, öffentliche Teile in `keys/secureboot/`), Einschreiben
durch systemd-boot, Secure Boot, LUKS2 + TPM, Recovery-Key, zweiter Start ohne Eingabe,
Windows unverändert. Neu installieren: `keys/secureboot/README.md` (`bz build`, `bz sign`,
`wsl --mount`, `bz install /dev/sde --guid "{6265697a-6500-4000-8000-000000000001}"
--reinstall`). Ziel-SSD: Samsung, Disk 0 in Windows; die SanDisk 480 GB (E:, Daten) und
die Corsair (Windows) nie anfassen. Hardware: ASUS ROG STRIX Z790-F GAMING WIFI, BIOS 3201,
BitLocker aus.

Erledigt in M2 (Details: ADR 0006–0010, Git-Log): signierte Quellen, CI mit Cache,
Secure Boot mit eigenem PK (Profil „Beize + Windows“), `/usr` mit dm-verity, A/B-Slots,
LUKS2 an PCR 7+12 + signierte PCR-11-Richtlinie „initrd“, `beize-tpm` (Neubinden nach
Recovery nur bei erlaubtem Zustand, Start-Prüfung PCR 12), Härtung für echte Hardware,
`bz install`, **Anmelden**: libxcrypt + Linux-PAM, login/sulogin, `os/beize-setup`
(Konto beim ersten Start; root bekommt denselben Hash für die Notfall-Konsole),
Konsole während Recovery-Key und Einrichtung stumm (`systemd-mute-console`), Terminus-
Schrift, **LAN** (ADR 0011: networkd + resolved, DHCP, nss-resolve, kein Fallback-DNS/NTP,
LLMNR aus, IPv6-Privatsphäre; beize-setup wartet per `tcdrain`, bis seine Ausgabe gesendet
ist – sonst verwarf `TTYReset` die letzte Zeile), **Zeit** (ADR 0012: ntpd-rs mit NTS gegen
PTB/Netnod/time.nl/Cloudflare, Mozilla-Zertifikate unter `/etc/ssl/cert.pem`, tzdata +
Zeitzonen-Frage in beize-setup; `bz` prüft dafür SSH-signierte Commits). In der VM
gemessen: synchronisiert nach ~2 min (8 Messungen à 16 s, bewusst nicht verkürzt).
Leere BIOS-Batterie: verifizierte Zeit wird stündlich gesichert (systemd-Epoch-Datei).
`bz smoke` grün, Start 5,28 s.

**Der Besitzer soll als Nächstes** neu bauen/signieren/installieren, beim ersten Start
sein Konto anlegen, LAN und Zeit prüfen (`networkctl`, `timedatectl`, `ntp-ctl status`) –
auf echter Hardware ungeprüft – und Windows einmalig auf UTC stellen
(`keys/secureboot/README.md` Schritt 4).

**Nächste Aufgaben für M2, in dieser Reihenfolge:**
1. **Startzeit auf echter Hardware messen** (`systemd-analyze` beim Besitzer), dann
   optimieren: initrd verschlanken (17 MB, ganzes systemd), Kernel zuschneiden.
2. **Updates über die A/B-Slots** (systemd-sysupdate) – damit ist M2 fertig. Wichtig auch,
   weil Zertifikatsliste und tzdata sonst auf dem Stand der Installation veralten.
3. Danach eine Aufräumrunde vor M3: `qemu.rs` (>1300 Zeilen) aufteilen, `~/probe/probe.py`
   und `~/probe/drive-run.py` (steuert `bz run`: Einrichtung beantworten, Befehle im Gast)
   als `bz debug` ins Repo, §12/ADR-Nachträge zu Endfassungen zusammenführen.

Offen und bewusst zurückgestellt: bit-genaue Builds (Binaries unterscheiden sich
zwischen Container- und lokalem Bau, vermutlich Build-Pfad), WLAN, NVIDIA (M3),
Rollback-Schutz für ältere signierte UKIs, TPM-Attestierung bei neuem TPM.
