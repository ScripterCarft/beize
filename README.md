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
- [x] Produktionsschlüssel (1Password) und Signieren für echte Hardware
- [x] Nur `/usr` wird ausgeliefert; die initrd legt beim ersten Start `/usr`-Slot B und die
      Root-Partition (`/etc`, `/var`, `/home`) an; Boot-Counting mit automatischem Rückfall
- [x] dm-verity für `/usr`: Root-Hash im signierten UKI, jeder Block wird beim Lesen geprüft
- [x] Root-Partition (`/etc`, `/var`, `/home`) mit LUKS2 verschlüsselt, an TPM2 und den
      Secure-Boot-Zustand gebunden; entsperrt ohne Eingabe nur auf genau diesem Rechner
- [x] Wiederherstellungsschlüssel: beim ersten Start erzeugt und einmal angezeigt
- [x] Netzwerk über LAN (systemd-networkd + resolved, DHCP, keine eingebauten
      Fremd-Server, IPv6-Privatsphäre); der Boot-Test prüft DHCP, DNS und Namensauflösung
- [x] Uhrzeit: authentifiziert per NTS (ntpd-rs), Zeitzonen, Zertifikatsspeicher
- [x] Läuft auf dem Rechner des Besitzers: Secure Boot, Verschlüsselung, Anmelden, LAN, Zeit
- [ ] Startzeit auf echter Hardware, Administratorrechte, Tastaturlayout
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
./bz build                  # Toolchain, Kernel, Pakete, /usr-Image, initrd und Disk bauen (nur Geändertes)
./bz smoke                  # Installieren, booten und alles prüfen, inkl. Gegentests (~75 s)
./bz smoke --quick          # Nur Installation und zwei Starts (~30 s)
./bz run                    # In QEMU starten; der Zustand bleibt bis zum nächsten Build erhalten
./bz debug 'networkctl'     # Befehle in der VM ausführen und ihre Ausgabe zeigen
./bz package systemd        # Ein Paket samt Abhängigkeiten bauen
./bz owner /usr/bin/bash    # Welches Paket hat diese Datei installiert?
./bz boot-times <journal>   # Wo ein Start Zeit verbraucht hat (docs/hardware.md)
./bz verify                 # Alle Quellen gegen Prüfsumme und Maintainer-Signatur prüfen
```

Einzelschritte: `./bz kernel`, `./bz usr`, `./bz initrd`, `./bz image`; `-v` zeigt alle Ausgaben.

`./bz smoke` installiert das Image wie auf einem PC mit gelöschten Secure-Boot-Schlüsseln,
startet es zweimal und prüft dabei alles, was ein Start leisten muss (Secure Boot, dm-verity,
Verschlüsselung, keine fehlgeschlagene Unit, Netz, Zeitzone, Recovery-Key genau einmal,
Zeitbudget je Startphase).
Danach laufen parallel, jeweils auf einer Kopie der Installation, die Gegentests: anderer TPM,
Secure Boot aus, Secure-Boot-Update, fremdes db-Zertifikat, hinzugefügter Kernel-Parameter,
unsigniertes Kernel-Image, verändertes `/usr`, und der Signier-Ablauf des Besitzers.
Einzelheiten: `tools/bz/src/vm/smoke.rs`.
Alles Erzeugte liegt unter `build/` und darf jederzeit gelöscht werden;
Logs in `build/work/logs/`.

## Aufbau

| Pfad | Inhalt |
| --- | --- |
| `tools/bz` | Build-Werkzeug: Rezepte → Toolchain, Pakete, `/usr`-Image, initrd, UKI, Disk |
| `recipes/` | Ein Rezept pro Paket: gepinnte, signierte Quelle, Abhängigkeiten, Build-Skript |
| `keys/` | Öffentliche Schlüssel der Upstream-Maintainer (siehe `keys/README.md`) |
| `ci/` | Paketliste und kompletter CI-Ablauf |
| `images/` | Welche Pakete in `/usr` und in die initrd gehören, welche Units laufen (ADR 0013) |
| `os/` | Eigener Rust-Code im Image (`beize-tpm`, `beize-setup`) |
| `system/usr/`, `system/initrd/` | Eigene Dateien in `/usr` und in der initrd (Units, Skripte, Werkseinstellungen) |
| `kernel/config/` | Kernel-Konfigurationsfragmente, nach dem Mergen verifiziert |
| `docs/adr/` | Architekturentscheidungen mit Begründung |
| `docs/hardware.md` | Rechner des Besitzers, Startzeit messen, Messwerte |

## Grundsätze

- Jede Upstream-Quelle ist auf Version und Prüfsumme (bzw. Commit) gepinnt und
  muss von ihren Maintainern signiert sein; unsignierte Quellen nimmt `bz` nicht an.
- Reproduzierbar angelegt (feste Zeitstempel, IDs, Metadaten). Noch nicht bit-gleich:
  Binaries aus Container- und lokalem Bau unterscheiden sich, und die Secure-Boot-
  Einschreibdateien tragen die Signierzeit.
- Private Schlüssel (Secure Boot, Signaturen) liegen nie im Repository, sondern in 1Password.
- Größere Entscheidungen werden als ADR festgehalten.
