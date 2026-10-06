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

Nächster Meilenstein: **M2 – Echte Hardware** (Secure Boot mit eigenen Schlüsseln,
A/B-Partitionen, persistentes `/var`, LUKS, NVIDIA, Netzwerk).

## Voraussetzungen

Entwickelt wird unter Linux, auf Windows in WSL2 (siehe [ADR 0004](docs/adr/0004-entwicklung-in-wsl2.md)).
Unter Ubuntu 26.04:

```sh
sudo apt install build-essential git curl pkg-config bison flex bc cpio kmod gawk texinfo \
  libelf-dev libssl-dev zstd xz-utils meson ninja-build gperf python3-jinja2 \
  qemu-system-x86 qemu-utils ovmf swtpm sbsigntool systemd-ukify systemd-boot-efi \
  mtools dosfstools fdisk erofs-utils
curl https://sh.rustup.rs -sSf | sh
```

Für schnelle VMs muss der Benutzer in der Gruppe `kvm` sein.

## Benutzung

```sh
./bz build                  # Toolchain, Kernel, Pakete, Root-Image und Disk bauen
./bz run                    # Image in QEMU starten (beenden mit Ctrl-A X)
./bz smoke                  # Image headless starten und prüfen, ob das System hochkommt
./bz package systemd        # Ein Paket samt Abhängigkeiten bauen
./bz owner /usr/bin/bash    # Welches Paket hat diese Datei installiert?
```

Einzelschritte: `./bz kernel`, `./bz rootfs`, `./bz image`.
Alles Erzeugte liegt unter `build/` und darf jederzeit gelöscht werden.
Build-Logs pro Paket: `build/work/logs/<paket>.log`.

## Aufbau

| Pfad | Inhalt |
| --- | --- |
| `tools/bz` | Build-Werkzeug: Rezepte → Toolchain, Pakete, Root-Image, UKI, Disk |
| `recipes/` | Ein Rezept pro Paket: gepinnte Quelle, Abhängigkeiten, Build-Skript |
| `images/` | Welche Pakete in ein Image gehören |
| `system/rootfs/` | Eigene Dateien im Root-Dateisystem (Konfiguration, systemd-Units) |
| `kernel/config/` | Kernel-Konfigurationsfragmente, nach dem Mergen verifiziert |
| `docs/adr/` | Architekturentscheidungen mit Begründung |

## Grundsätze

- Jede Upstream-Quelle ist auf Version und Prüfsumme gepinnt.
- Gleicher Quellstand ergibt das gleiche Image (feste Zeitstempel, IDs, Metadaten).
- Schlüssel liegen nie im Repository, sondern in 1Password.
- Größere Entscheidungen werden als ADR festgehalten.
