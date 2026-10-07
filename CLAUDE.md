# Beize – Übergabe für Claude Code

Diese Datei ist der Einstieg für jede neue Session. Sie ersetzt den bisherigen
Chatverlauf. Stand: 2026-10-07, Commit `78bc260` (+ diese Übergabe).

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
  `gh` ist nicht installiert; CI-Status sieht der Besitzer unter „Actions“.
- Läuft Claude Code auf Windows (Arbeitsverzeichnis `F:\code-projects`), Befehle so ausführen:
  `WSL_UTF8=1 wsl.exe -d Ubuntu-26.04 -- bash -l -s <<'EOF' … EOF` (Heredoc!
  `bash -c '…'` wird umgeparst, `$?`/`$(…)` expandieren falsch; Git Bash
  wandelt `/tmp/…`-Argumente in Windows-Pfade).

## 4. Architektur (Stand jetzt)

```
Firmware (Secure Boot, eigene PK/KEK/db)
  └─ prüft systemd-boot (signiert, eigene SBAT „beize“)
       └─ prüft UKI beize_<ver>+3.efi  = Kernel + initrd + Kommandozeile (signiert)
            Kommandozeile: usrhash=<root hash> mount.usr=/dev/mapper/usr …
            └─ initrd (systemd): beim 1. Start systemd-repart → Slot B + Root anlegen,
               dm-verity für /usr, Root auf der Boot-Disk finden (gpt-auto), switch-root
                 └─ System: /usr (EROFS, dm-verity, read-only) + / (ext4: /etc, /var, /home)
```

| Partition (GPT, UAPI.2-Typen) | Inhalt | Entsteht |
| --- | --- | --- |
| ESP 512 MiB (FAT32, 4-KiB-Cluster) | systemd-boot, UKIs, `loader/loader.conf` | Image |
| `/usr` A data 1 GiB + verity 64 MiB, Label `beize_<ver>` | System; UUIDs = Hälften des Root-Hashes | Image |
| `/usr` B data + verity, Label `_empty` | nächstes Update | 1. Start |
| Root (ext4, `FactoryReset=yes`), wächst in den Rest | alles des Besitzers | 1. Start |

Nur `/usr` wird ausgeliefert; `/etc` wird beim 1. Start aus
`/usr/share/factory/etc` (tmpfiles) und von systemd erzeugt. Boot-Counting:
3 Versuche, `systemd-bless-boot` entfernt den Zähler. Alle Größen, Typen,
Versionen und die repart-Definitionen stehen **nur** in `tools/bz/src/layout.rs`.

## 5. Repository

| Pfad | Inhalt |
| --- | --- |
| `tools/bz/src/` | Build-Werkzeug (Rust, ~3300 Zeilen): `main` CLI · `source` Download/Pin/Signatur · `recipe` Rezepte · `packages` Abhängigkeiten, Fingerprints, Build-Umgebung · `install` Sysroot, merged-/usr, Besitz-Manifeste · `kernel` · `layout` Disk-Layout · `tree` Dateibäume, strip · `usr` /usr-Image + dm-verity · `initrd` · `secureboot` Testschlüssel, Signieren, OVMF-Variablen · `image` UKI + Disk · `qemu` `run`/`smoke` · `util` |
| `recipes/*.toml` | ein Rezept pro Paket (19): Quelle (gepinnt + signiert), `deps`, `stage` (toolchain/target), `[build] script` |
| `keys/` | öffentliche Maintainer-Schlüssel + `README.md` (Herkunft, Fingerprints, Ausnahmen) |
| `images/base.toml` | Pakete für `/usr` und für die initrd |
| `system/usr/` | eigene Dateien in `/usr` (Units, Boot-Report, Werkseinstellungen) |
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
```

Logs: `build/work/logs/<paket>.log`. Voller Neubau ~10–15 min (GCC zweimal),
inkrementell Sekunden bis wenige Minuten. `./bz smoke` installiert das Image auf
eine frische 8-GiB-Disk und verlangt: Secure Boot an, `/usr` über dm-verity,
`running`, **keine `[FAILED]`/`[DEPEND]`-Zeile in initrd oder System**, sauberes
Herunterfahren, 1. Start legt alle Partitionen an (Typ + Label geprüft),
Boot-Counting „gut“, 2. Start = gleiche Machine-ID; Gegentests: unsigniertes UKI
→ „Access denied“, ein verändertes Byte in `/usr` → „data block … is corrupted“.
Aktuell: Start ~5 s Gast-Uptime (Kernel 0,6 + initrd 2,3 + System 2,2 s).

CI (`.github/workflows/ci.yml`): fmt/clippy/test bei jedem Push; auf `main`,
manuell und wöchentlich `ci/system.sh` in privilegiertem `ubuntu:26.04`-Container
mit KVM (verify, build, smoke). Ob der GitHub-Lauf grün ist, wurde noch nicht bestätigt.

## 7. Sicherheitsmodell

- **Quellen:** jede gepinnt (SHA-256 bzw. Commit) **und** von Upstream signiert,
  geprüft mit `gpgv` gegen `keys/<name>.gpg`. Ohne Signatur nur mit expliziter
  Begründung `unsigned = "…"` (einzige Ausnahme: json-c). systemd kommt aus dem
  signierten Git-Tag.
- **Build:** eigene Cross-Toolchain (`x86_64-beize-linux-gnu`), kein Host-Header im
  System; reproduzierbar (fester Zeitstempel `BUILD_EPOCH`, feste UUIDs, Salt aus Inhalt).
- **Boot:** Secure Boot → systemd-boot → UKI (Kommandozeile versiegelt, `editor no`)
  → `usrhash` → dm-verity für jeden `/usr`-Block.
- **Schlüssel:** Tests/CI mit Wegwerf-Schlüsseln `CN=Beize TEST ONLY …` unter
  `build/work/secureboot-test/`. Produktionsschlüssel nur beim Besitzer (1Password,
  Tresor „Beize“); für echte Hardware signiert er selbst. Diese Schritte
  (`bz secureboot new-keys`, `bz sign`, Einschreiben in die Firmware) sind **noch nicht gebaut**.

## 8. ADRs (`docs/adr/`)

0001 Linux-Kernel aus Upstream · 0002 glibc + systemd · 0003 unveränderliches
signiertes Image · 0004 Entwicklung in WSL2 · 0005 Cross-Toolchain, Rezepte,
signierte Quellen, Besitz-Manifeste · 0006 Secure Boot, Test- vs.
Produktionsschlüssel · 0007 nur `/usr`, A/B, Einrichtung beim ersten Start ·
0008 dm-verity für `/usr`. Neue Architekturentscheidung = neues ADR.

## 9. Regeln für Rezepte und Code

- Nur `/usr` ausliefern: `bz` bricht ab bei Dateien außerhalb von `/usr`; Altlasten
  im Rezept löschen (mit Kommentar) oder nach `/usr/share/factory` verlegen.
- **pkg-config liefert beim Cross-Build Pfade mit Sysroot-Präfix** (Unit-, udev-,
  tmpfiles-, sysusers-Verzeichnisse). Solche Verzeichnisse immer explizit setzen;
  `bz` bricht ab, wenn etwas unter `home/…/build/` installiert würde.
- `meson -Dauto_features=disabled`: jedes benötigte Feature ausdrücklich einschalten.
- Abhängigkeitsschleifen vermeiden (libdevmapper ohne udev-Sync, weil systemd → cryptsetup → lvm2).
- Build-Jobs werden nach RAM begrenzt (1,5 GiB/Job) – nicht zurückdrehen.
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
- FAT32 braucht ≥ 65 525 Cluster (Compile-Time-Check in `layout.rs`); beim
  Installieren auf größere Disks Backup-GPT verschieben (`sfdisk --relocate gpt-bak-std`).
- systemd-repart lässt Partitionen ohne `SizeMaxBytes=` wachsen – auch bestehende
  (Test `only_root_grows`). Vorab prüfen: `systemd-repart --definitions=… --dry-run=yes <Kopie>`.

## 11. Offene Punkte (außer der aktuellen Aufgabe)

- Notfall-Shell geht nicht (`sulogin` fehlt; braucht libxcrypt → mit Benutzerpasswörtern).
- initrd 15,9 MiB / 2,3 s – enthält noch ganz systemd; verschlanken. hwdb nicht generiert.
- Compiler-Flags generisch (`-O2 -pipe`); Ziel x86-64-v3 + Raptor-Lake-Tuning, Kernel auf Hardware zuschneiden.
- Produktionsschlüssel-Workflow, Updater (systemd-sysupdate), Verhalten bei
  verity-Fehlern zur Laufzeit (Neustart → Rückfall?), Netzwerk, NVIDIA, Bluetooth,
  Start von der zweiten SSD, Installer.

## 12. AKTUELLE AUFGABE: Root-Partition mit LUKS2 + TPM2

**Ziel:** Die Root-Partition (`/etc`, `/var`, `/home`) wird beim ersten Start von
systemd-repart in der initrd mit LUKS2 verschlüsselt und an den TPM gebunden; jeder
Start entsperrt automatisch ohne Eingabe – aber nur auf genau diesem Rechner mit
intaktem Secure Boot.

**Schritte (Vorschlag, in dieser Reihenfolge):**
1. **tpm2-tss** als neues Paket (github.com/tpm2-software/tpm2-tss, Release-Archiv
   + `.asc` – Signatur und Schlüssel wie üblich prüfen, Schlüssel in eigenem Commit).
   `--disable-fapi` (zieht sonst libcurl), Doku aus; udev-, sysusers- und
   tmpfiles-Verzeichnisse **explizit** setzen (pkg-config-Falle).
2. **systemd:** `-Dtpm2=enabled -Dopenssl=enabled`, `tpm2-tss` in `deps`.
   systemd lädt die tss2-Bibliotheken per dlopen – prüfen, dass sie in `/usr` **und**
   in der initrd landen (initrd-Liste in `images/base.toml` ggf. ergänzen).
3. **Kernel:** `CONFIG_TCG_TPM`, `CONFIG_TCG_CRB` (QEMU `tpm-crb`, Intel PTT),
   `CONFIG_TCG_TIS`, `CONFIG_HW_RANDOM_TPM`, `CONFIG_SECURITYFS`; `CONFIG_DM_CRYPT` ist schon an.
4. **Layout:** in `layout.rs` `20-root.conf` um `Encrypt=tpm2` ergänzen; PCR-Bindung
   bewusst wählen (Startvorschlag `TPM2PCRs=7` = Secure-Boot-Zustand, signierte
   PCR-11-Richtlinie später) und als **ADR 0009** begründen. gpt-auto setzt für
   verschlüsseltes Root bereits `tpm2-device=auto` (geprüft in systemd 262).
5. **QEMU:** `swtpm` pro `Machine` (eigener State-Ordner, bleibt zwischen den beiden
   Starts erhalten): `swtpm socket --tpm2 --tpmstate dir=… --ctrl type=unixio,path=…`
   + `-chardev socket,id=chrtpm,path=… -tpmdev emulator,id=tpm0,chardev=chrtpm -device tpm-crb,tpmdev=tpm0`.
   **`swtpm` und `swtpm-tools` fehlen in `ci/packages.txt`** – ergänzen.
6. **Boot-Report:** Feld `root=encrypted`, wenn `/` von einem dm-Gerät mit
   UUID `CRYPT-LUKS2-*` kommt (analog zur `usr=verity`-Prüfung).
7. **Smoke-Test:** verlangt `root=encrypted` bei beiden Starts, kein Passwort-Prompt.
   **Gegentest:** dieselbe Disk mit **frischem** TPM-Zustand → Root darf nicht
   aufgehen (Passwortabfrage/Fehlschlag, kein Bericht). Optional: anderer
   Secure-Boot-Zustand (PCR 7) → ebenfalls kein Entsperren.
8. ADR 0009, README-Stand, `CLAUDE.md` aktualisieren, committen, pushen.

**Vor echter Hardware zwingend klären (nicht für den VM-Test):** Wiederherstellung.
Ändert sich PCR 7 (Firmware-Update, Schlüsselwechsel) oder stirbt der TPM, sind die
Daten ohne Recovery-Key verloren. Plan nötig: Recovery-Key per
`systemd-cryptenroll --recovery-key` im Onboarding erzeugen und dem Besitzer
anzeigen (nicht automatisch speichern – 1Password ist für Claude tabu). Mit dem
Besitzer abstimmen.

**Fertig, wenn:** `cargo test`/clippy grün, `./bz verify` ok, `./bz smoke` grün inkl.
neuem Gegentest, keine `[FAILED]`-Zeile, Boot-Zeit gemessen und im Bericht an den
Besitzer genannt.
