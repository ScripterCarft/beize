# Beize

Ein eigenes Betriebssystem auf dem Linux-Kernel: so konsequent wie macOS, so frei
wie Windows, technisch so offen wie Linux. Kernel, Treiber und Basisbibliotheken
kommen aus Upstream, alles darüber ist eigener Code.

Vision und Designprinzipien: [`docs/VISION.md`](docs/VISION.md) (Original als
[Artifact](https://claude.ai/code/artifact/1204e9ff-abc6-44b7-8320-17b195f5a226)).
Einstieg für Entwicklung mit Claude Code: [`CLAUDE.md`](CLAUDE.md).

## Stand

Meilenstein **M1 – Bootet in QEMU** ist erreicht.

- [x] Build-Werkzeug `bz` mit gepinnten, geprüften Quellen
- [x] Kernel aus dem Quellcode mit eigener, verifizierter Konfiguration
- [x] Eigene Cross-Toolchain (binutils, gcc) und glibc aus dem Quellcode
- [x] systemd, D-Bus, bash, coreutils, util-linux aus dem Quellcode
- [x] Unified Kernel Image auf GPT-Disk, Start über UEFI
- [x] Shell auf der seriellen Konsole in QEMU
- [x] Automatischer Boot-Test: systemd `running`, keine fehlgeschlagene Unit

Meilenstein **M2 – Echte Hardware** ist in Arbeit.

- [x] Alle Quellen gepinnt und von ihren Maintainern signiert (`bz verify`)
- [x] CI baut und bootet das komplette System
- [x] Secure Boot: signiertes systemd-boot + UKI, eigene Schlüssel; der Boot-Test
      läuft unter erzwungenem Secure Boot und prüft, dass Unsigniertes abgewiesen wird
- [ ] Produktionsschlüssel (1Password) und Signieren für echte Hardware
- [x] Nur `/usr` wird ausgeliefert; die initrd legt beim ersten Start `/usr`-Slot B und die
      Root-Partition (`/etc`, `/var`, `/home`) an; Boot-Counting mit automatischem Rückfall
- [x] dm-verity für `/usr`: Root-Hash im signierten UKI, jeder Block wird beim Lesen geprüft
- [x] Root-Partition (`/etc`, `/var`, `/home`) mit LUKS2 verschlüsselt, an TPM2 und den
      Secure-Boot-Zustand gebunden; entsperrt ohne Eingabe nur auf genau diesem Rechner
- [x] Wiederherstellungsschlüssel: beim ersten Start erzeugt und einmal angezeigt
- [ ] Updater (systemd-sysupdate)
- [ ] Kernel und Compiler-Flags für i7-13700K / Z790, NVIDIA, Netzwerk, Bluetooth
- [ ] Start von der zweiten SSD

## Voraussetzungen

Entwickelt wird unter Linux, auf Windows in WSL2 (siehe [ADR 0004](docs/adr/0004-entwicklung-in-wsl2.md)).
Unter Ubuntu 26.04 – dieselbe Paketliste nutzt auch die CI:

```sh
grep -vE '^\s*(#|$)' ci/packages.txt | xargs sudo apt install -y
curl https://sh.rustup.rs -sSf | sh
```

Für schnelle VMs muss der Benutzer in der Gruppe `kvm` sein.

`ci/system.sh` führt den kompletten Ablauf der CI aus (Quellen prüfen, alles
bauen, booten) und läuft in einem frischen Ubuntu-26.04-Container, z. B.:
`podman run --rm --privileged --device /dev/kvm -v "$PWD:/src" -w /src ubuntu:26.04 ci/system.sh`

## Benutzung

```sh
./bz build                  # Toolchain, Kernel, Pakete, /usr-Image, initrd und Disk bauen
./bz run                    # In QEMU starten; der Zustand bleibt bis zum nächsten Build erhalten
./bz smoke                  # Auf frischer Disk zweimal booten und alles prüfen (siehe unten)
./bz package systemd        # Ein Paket samt Abhängigkeiten bauen
./bz owner /usr/bin/bash    # Welches Paket hat diese Datei installiert?
./bz verify                 # Alle Quellen gegen Prüfsumme und Maintainer-Signatur prüfen
```

Einzelschritte: `./bz kernel`, `./bz usr`, `./bz initrd`, `./bz image`.

`./bz smoke` installiert das Image auf eine frische 8-GB-Test-Disk und prüft:
Secure Boot aktiv, `/usr` über dm-verity, Root-Partition verschlüsselt und per TPM
entsperrt, erster Start legt Partitionen an, systemd `running` ohne eine
einzige fehlgeschlagene Unit (auch in der initrd), Boot-Counting markiert den Start
als gut, der zweite Start findet dieselbe Installation, der Recovery-Key wird genau
einmal angezeigt – und vier Gegentests: mit einem anderen TPM bleibt die
Root-Partition verschlossen (der Recovery-Key öffnet sie), mit abgeschaltetem
Secure Boot ebenfalls, ein unsigniertes Kernel-Image wird von der Firmware abgewiesen, ein
einziges verändertes Byte in `/usr` wird von dm-verity erkannt.
Alles Erzeugte liegt unter `build/` und darf jederzeit gelöscht werden.
Build-Logs pro Paket: `build/work/logs/<paket>.log`.

## Aufbau

| Pfad | Inhalt |
| --- | --- |
| `tools/bz` | Build-Werkzeug: Rezepte → Toolchain, Pakete, `/usr`-Image, initrd, UKI, Disk |
| `recipes/` | Ein Rezept pro Paket: gepinnte, signierte Quelle, Abhängigkeiten, Build-Skript |
| `keys/` | Öffentliche Schlüssel der Upstream-Maintainer (siehe `keys/README.md`) |
| `ci/` | Paketliste und kompletter CI-Ablauf |
| `images/` | Welche Pakete in `/usr` und in die initrd gehören |
| `system/usr/` | Eigene Dateien in `/usr` (Units, Werkseinstellungen für `/etc`) |
| `kernel/config/` | Kernel-Konfigurationsfragmente, nach dem Mergen verifiziert |
| `docs/adr/` | Architekturentscheidungen mit Begründung |

## Grundsätze

- Jede Upstream-Quelle ist auf Version und Prüfsumme (bzw. Commit) gepinnt und
  muss von ihren Maintainern signiert sein; unsignierte Quellen nimmt `bz` nicht an.
- Gleicher Quellstand ergibt das gleiche Image (feste Zeitstempel, IDs, Metadaten).
- Private Schlüssel (Secure Boot, Signaturen) liegen nie im Repository, sondern in 1Password.
- Größere Entscheidungen werden als ADR festgehalten.
