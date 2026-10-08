# Secure-Boot-Schlüssel für echte Hardware

Hier liegen nur **öffentliche** Dateien der Produktionsschlüssel des Besitzers
(ADR 0010). Die privaten Teile liegen ausschließlich im 1Password-Tresor „Beize“
und existieren auf dem Build-Rechner nur im Arbeitsspeicher, während `bz` sie
braucht. Tests und CI benutzen eigene Wegwerf-Schlüssel und brauchen nichts von hier.

| Datei | Inhalt |
| --- | --- |
| `PK.crt`, `KEK.crt`, `db.crt` | Zertifikate von Plattform-, Schlüsselaustausch- und Signaturschlüssel |
| `pcr.pub.pem` | öffentlicher PCR-Schlüssel – steckt als `.pcrpkey` in jedem UKI |
| `PK.auth`, `KEK.auth`, `db.auth`, `dbx.auth` | signierte Firmware-Updates, die systemd-boot einschreibt (Profil „Beize + Windows“: inklusive Microsofts Zertifikaten und Sperrliste) |

Alle Befehle unten führt **nur der Besitzer** aus, in einem eigenen Terminal
(WSL). `bz` verweigert sie ohne Terminal – Automatisierung und Claude kommen
nicht an die Schlüssel.

## Voraussetzung: 1Password-CLI

Unter WSL spricht nur die **Windows**-CLI mit der 1Password-App:
1. 1Password-CLI für Windows installieren (`winget install AgileBits.1Password.CLI`).
2. In der 1Password-App: Einstellungen → Entwickler → „Mit 1Password CLI integrieren“.
3. In WSL prüfen: `op.exe vault list` zeigt den Tresor „Beize“ (Windows Hello fragt).

## 1. Schlüssel erzeugen (einmalig)

```sh
cd ~/beize
./bz secureboot new-keys
git add keys/secureboot && git commit -m "Add the production Secure Boot keys (public parts)"
```

`bz` erzeugt PK, KEK, db und PCR-Schlüssel im Arbeitsspeicher, legt die privaten
Teile als vier Dokumente „Beize … key …“ im Tresor „Beize“ ab, liest sie zur
Kontrolle zurück und schreibt Zertifikate und `.auth`-Dateien hierher. Gibt es
sie schon, bricht `bz` ab – ein Schlüsselwechsel ist ein eigener, bewusster Schritt.

## 2. Image signieren (bei jeder Installation und jedem Update)

```sh
./bz build
./bz sign      # → build/out/production/disk.img
```

Braucht nur db- und PCR-Schlüssel (1Password fragt per Windows Hello). `bz sign`
prüft, dass sie zu den Dateien hier passen, und löscht sie danach wieder aus dem
Arbeitsspeicher.

## 3. Schlüssel in die Firmware (einmalig, und nach BIOS-Updates)

Beize schreibt die Schlüssel **selbst** ein – im BIOS werden sie nur gelöscht:

1. Vorher: Ist BitLocker unter Windows an, den Wiederherstellungsschlüssel
   bereithalten (Stand 2026-10-08: BitLocker aus).
2. BIOS (`Entf`) → `F7` Advanced Mode → **Boot → Secure Boot → Key Management →
   „Clear Secure Boot Keys“** (Name kann je BIOS-Version leicht abweichen). Die
   Firmware ist danach im Setup-Modus. **Ein BIOS-Administrator-Passwort setzen**,
   falls noch keins da ist – sonst kann jeder mit Zugang zum Rechner das auch.
3. Von der Beize-SSD starten: systemd-boot zeigt „Enrolling secure boot keys …“
   und 15 Sekunden Zeit zum Abbrechen (beliebige Taste), schreibt dann PK, KEK,
   db und dbx ein und startet neu.
4. Prüfen: Beize startet, Windows startet weiterhin mit Secure Boot.

**ASUS setzt bei BIOS-Updates die Schlüssel oft auf Werkszustand zurück.** Dann
startet Beize nicht unter Secure Boot (die Daten bleiben verschlossen) – Schritt 3
wiederholen.
