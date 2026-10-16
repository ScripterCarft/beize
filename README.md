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
- [x] systemd, D-Bus (dbus-broker), bash, coreutils, util-linux aus dem Quellcode
- [x] Unified Kernel Image auf GPT-Disk, Start über UEFI
- [x] Shell auf der seriellen Konsole in QEMU
- [x] Automatischer Boot-Test: systemd `running`, keine fehlgeschlagene Unit

Meilenstein **M2 – Echte Hardware** ist erreicht (2026-10-16, Beize 0.4).

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
- [x] Was läuft, steht im signierten Image; der Boot-Test prüft jede laufende Unit
- [x] Alles für x86-64-v3 gebaut und gehärtet (FORTIFY, CET, Stack-Clash, RELRO/now);
      `bz` prüft jede ELF-Datei im Image
- [x] Kernel mit eigener Konfiguration auf Upstreams Standardwerten, Treiber als
      signierte Module (nur signierte werden geladen, Lockdown), CPU-Microcode im UKI;
      der Boot-Test prüft, dass ein unsigniertes Modul abgewiesen wird
- [x] Tastaturlayout: beim ersten Start gewählt (XKB), auf der Konsole und in der initrd –
      der Recovery-Key wird mit der eigenen Tastatur getippt
- [x] Administratorrechte mit run0 und dem eigenen Passwort (polkit), root ohne Passwort;
      keine Datei mit setuid oder Capabilities, `/` und `/usr` mit `nosuid` eingehängt;
      `ping` und `tracepath` ohne Sonderrechte; Dienste mit Systemaufruf-Filtern (seccomp)
- [x] Wiederherstellung im Startmenü: Root-Shell nur mit dem Wiederherstellungsschlüssel
- [x] Startzeit auf echter Hardware gemessen: 13,6 s vom Bootloader bis zur Anmeldung, davon
      5 s PCIe-Retraining, das Kernel 7.3 behebt ([`docs/hardware.md`](docs/hardware.md))
- [x] A/B-Updates: signiert (ML-DSA), aktiv beim nächsten Start; ein Update, das nicht
      startet, fällt nach drei Versuchen von selbst auf die vorige Version zurück
      (auf der Hardware bestätigt: 0.1 → 0.2 → 0.3 → 0.4 vom USB-Stick)
- [x] Über Stunden im Leerlauf stabil: nichts wächst, ~750 MB belegt, davon 26 MB anonymer
      Speicher aller Prozesse

Meilenstein **M3 – Compositor** ist der nächste. Dazu gehört auch, NVIDIA, WLAN, Bluetooth,
Ton und den Start von der zweiten SSD zu planen.

## Voraussetzungen

Entwickelt wird unter Linux, auf Windows in WSL2 (siehe [ADR 0004](docs/adr/0004-entwicklung-in-wsl2.md)).
`bz` baut nur in Beizes Buildroot ([ADR 0021](docs/adr/0021-buildroot.md)): Ubuntu 26.04
mit gepinnten Paketen und Rust, dieselbe Umgebung wie in der CI. `./bz` legt ihn beim ersten
Aufruf an (~4 min). Auf dem Rechner selbst braucht es dafür nur:

```sh
sudo apt install mmdebstrap bubblewrap curl   # Ubuntu/Debian
```

Dazu Untergeordnete IDs für den Benutzer (`/etc/subuid`, `/etc/subgid`, unter Ubuntu
Standard) und für schnelle VMs die Gruppe `kvm`. Pakete des Buildroots ändern:
`ci/buildroot/packages.txt` bearbeiten, dann `ci/buildroot/lock.sh`.

`ci/system.sh` ist der komplette Ablauf der CI (Quellen prüfen, alles bauen, booten).

## Benutzung

```sh
./bz build                  # Toolchain, Kernel, Pakete, /usr-Image, initrd und Disk bauen (nur Geändertes)
./bz smoke                  # Installieren, booten und alles prüfen, inkl. Gegentests (~2 min)
./bz run                    # In QEMU starten; der Zustand bleibt bis zum nächsten Build erhalten
./bz debug 'networkctl'     # Befehle in der VM ausführen und ihre Ausgabe zeigen
```

Alle Befehle: [`CLAUDE.md`](CLAUDE.md) §6; `./bz --help`.

`./bz smoke` installiert das Image wie auf einem PC mit gelöschten Secure-Boot-Schlüsseln,
startet es zweimal und prüft dabei alles, was ein Start leisten muss (Secure Boot, dm-verity,
Verschlüsselung, keine fehlgeschlagene Unit, Netz, Zeitzone, Recovery-Key genau einmal,
Zeitbudget je Startphase).
Danach laufen parallel, jeweils auf einer Kopie der Installation, die Gegentests: anderer TPM,
Secure Boot aus, Secure-Boot-Update, fremdes db-Zertifikat, hinzugefügter Kernel-Parameter,
unsigniertes Kernel-Image, verändertes `/usr`, der Signier-Ablauf des Besitzers, Updates
(abgewiesene, gelungene, kaputte mit Rückfall auf die vorige Version).
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
| `images/` | Welche Pakete in `/usr` gehören, aus welchen Funktionen die initrd besteht (ADR 0016), welche Units laufen (ADR 0013) |
| `os/` | Eigener Rust-Code im Image (`beize-tpm`, `beize-setup`, `beize-accounts`, `beize-update`, `beize-varlink`) |
| `system/usr/`, `system/initrd/` | Eigene Dateien in `/usr` und in der initrd (Units, Skripte, Werkseinstellungen) |
| `kernel/config/` | Kernel-Konfiguration nach Funktion (ADR 0015); `kernel/beize.config` ist das aufgelöste Ergebnis |
| `docs/adr/` | Architekturentscheidungen mit Begründung |
| `docs/hardware.md` | Rechner des Besitzers, Startzeit messen, Messwerte |

## Grundsätze

- Jede Upstream-Quelle ist auf Version und Prüfsumme (bzw. Commit) gepinnt und, wo
  ihre Maintainer signieren, gegen deren Signatur geprüft; `bz verify` nennt die
  wenigen unsignierten mit Grund.
- Reproduzierbar: derselbe Stand ergibt dasselbe Disk-Image, Bit für Bit (feste Zeitstempel,
  IDs, Signierzeit; kein Pfad der Baumaschine im Image) – solange der Kernel nicht neu gebaut
  wird: seine Module signiert ein Wegwerf-Schlüssel je Bau (Issue #5).
- Private Schlüssel (Secure Boot, Signaturen) liegen nie im Repository, sondern in 1Password.
- Größere Entscheidungen werden als ADR festgehalten.

## Lizenz

Beizes eigener Code steht unter der [Mozilla Public License 2.0](LICENSE). Die Pakete, aus
denen Beize gebaut wird, behalten ihre Lizenzen; welche, steht in der SBOM jedes Images
(`build/out/sbom.spdx.json`, ADR 0022).
