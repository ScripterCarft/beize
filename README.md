# Beize

Ein eigenes Betriebssystem auf dem Linux-Kernel: so konsequent wie macOS, so frei
wie Windows, technisch so offen wie Linux. Kernel, Treiber und Basisbibliotheken
kommen aus Upstream, alles darüber ist eigener Code.

Vision und Designprinzipien: [OS-Vision](https://claude.ai/code/artifact/1204e9ff-abc6-44b7-8320-17b195f5a226)

## Stand

Meilenstein **M1 – Bootet in QEMU** ist erreicht.

- [x] Build-Werkzeug `bz` mit gepinnten, geprüften Quellen
- [x] Kernel aus dem Quellcode mit eigener, verifizierter Konfiguration
- [x] Eigene Cross-Toolchain (binutils, gcc) und glibc aus dem Quellcode
- [x] systemd, D-Bus, bash, coreutils, util-linux aus dem Quellcode
- [x] Read-only Root (EROFS) auf GPT-Disk, Unified Kernel Image, Start über UEFI
- [x] Shell auf der seriellen Konsole in QEMU
- [x] Automatischer Boot-Test: systemd `running`, keine fehlgeschlagene Unit

Meilenstein **M2 – Echte Hardware** ist in Arbeit.

- [x] Alle Quellen gepinnt und von ihren Maintainern signiert (`bz verify`)
- [x] CI baut und bootet das komplette System
- [x] Secure Boot: signiertes systemd-boot + UKI, eigene Schlüssel; der Boot-Test
      läuft unter erzwungenem Secure Boot und prüft, dass Unsigniertes abgewiesen wird
- [ ] Produktionsschlüssel (1Password) und Signieren für echte Hardware
- [ ] A/B-Systempartitionen mit Rückfall, persistentes `/var`, LUKS + TPM2
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
./bz build                  # Toolchain, Kernel, Pakete, Root-Image und Disk bauen
./bz run                    # Image in QEMU starten (beenden mit Ctrl-A X)
./bz smoke                  # Booten unter Secure Boot prüfen (und dass Unsigniertes abgewiesen wird)
./bz package systemd        # Ein Paket samt Abhängigkeiten bauen
./bz owner /usr/bin/bash    # Welches Paket hat diese Datei installiert?
./bz verify                 # Alle Quellen gegen Prüfsumme und Maintainer-Signatur prüfen
```

Einzelschritte: `./bz kernel`, `./bz rootfs`, `./bz image`.
Alles Erzeugte liegt unter `build/` und darf jederzeit gelöscht werden.
Build-Logs pro Paket: `build/work/logs/<paket>.log`.

## Aufbau

| Pfad | Inhalt |
| --- | --- |
| `tools/bz` | Build-Werkzeug: Rezepte → Toolchain, Pakete, Root-Image, UKI, Disk |
| `recipes/` | Ein Rezept pro Paket: gepinnte, signierte Quelle, Abhängigkeiten, Build-Skript |
| `keys/` | Öffentliche Schlüssel der Upstream-Maintainer (siehe `keys/README.md`) |
| `ci/` | Paketliste und kompletter CI-Ablauf |
| `images/` | Welche Pakete in ein Image gehören |
| `system/rootfs/` | Eigene Dateien im Root-Dateisystem (Konfiguration, systemd-Units) |
| `kernel/config/` | Kernel-Konfigurationsfragmente, nach dem Mergen verifiziert |
| `docs/adr/` | Architekturentscheidungen mit Begründung |

## Grundsätze

- Jede Upstream-Quelle ist auf Version und Prüfsumme (bzw. Commit) gepinnt und
  muss von ihren Maintainern signiert sein; unsignierte Quellen nimmt `bz` nicht an.
- Gleicher Quellstand ergibt das gleiche Image (feste Zeitstempel, IDs, Metadaten).
- Private Schlüssel (Secure Boot, Signaturen) liegen nie im Repository, sondern in 1Password.
- Größere Entscheidungen werden als ADR festgehalten.
