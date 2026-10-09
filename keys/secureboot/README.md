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
Arbeitsspeicher. Dieses Image ist für echte Hardware gebaut: Konsole auf dem
Bildschirm, keine Credentials aus der Firmware, keine Testhelfer.

## 2b. Auf die Ziel-SSD schreiben (einmalig pro Installation)

Ziel ist eine **leere** SSD. Auf dem Rechner des Besitzers (Stand 2026-10-09): die
**Samsung SSD 860 EVO 1TB**, GPT-GUID `{febded67-6af6-43e1-81b7-73f9309df929}`.
Achtung: Die Windows-SSD (Corsair) ist auf das Byte gleich groß – erkannt wird die
richtige Platte nur an der GUID, und genau die prüft `bz install`.

1. PowerShell **als Administrator**: `Get-Disk` – Zeile „Samsung SSD 860 EVO 1TB“,
   Nummer (`Number`) und `Guid` notieren. Die Nummer kann sich nach einem Neustart
   ändern, die GUID nicht.
2. Ebenfalls dort: `wsl --mount \\.\PHYSICALDRIVE<Nummer> --bare` – die Platte
   erscheint in WSL, Windows greift solange nicht darauf zu.
3. In WSL: `lsblk -o NAME,SIZE,MODEL` – die neue Platte suchen (z. B. `/dev/sde`).
4. `./bz install /dev/sde --guid "{febded67-6af6-43e1-81b7-73f9309df929}"`
   `bz` bricht ab, wenn die GUID nicht passt, etwas eingehängt ist oder auf der
   Platte mehr als eine leere Microsoft-Verwaltungspartition liegt. Es zeigt Modell,
   Größe und GUID und schreibt erst nach `yes` (fragt einmal nach dem sudo-Passwort).
5. PowerShell (Administrator): `wsl --unmount \\.\PHYSICALDRIVE<Nummer>`.

Windows zeigt die SSD danach mit unbekannten Partitionen an – **nicht formatieren
lassen**, falls es fragt.

## 3. Schlüssel in die Firmware (einmalig, und nach BIOS-Updates)

Beize schreibt die Schlüssel **selbst** ein – im BIOS werden sie nur gelöscht:

1. Vorher: Ist BitLocker unter Windows an, den Wiederherstellungsschlüssel
   bereithalten (Stand 2026-10-08: BitLocker aus).
2. BIOS (`Entf`) → `F7` Advanced Mode → **Boot → Secure Boot → Key Management →
   „Clear Secure Boot Keys“** (Name kann je BIOS-Version leicht abweichen). Die
   Firmware ist danach im Setup-Modus. (Ein BIOS-Passwort ist optional: Wer im
   BIOS Secure Boot abschaltet oder Schlüssel ändert, verändert PCR 7 – der TPM
   gibt die Beize-Daten dann nicht frei, wie bei BitLocker. Es schützt zusätzlich
   gegen Manipulation, wenn Fremde Zugang zum Rechner haben.)
3. Von der Beize-SSD starten: beim Einschalten `F8` (ASUS-Bootmenü) → den
   UEFI-Eintrag der Samsung-SSD wählen. systemd-boot zeigt „Enrolling secure boot
   keys …“ und 15 Sekunden Zeit zum Abbrechen (beliebige Taste), schreibt dann PK,
   KEK, db und dbx ein und startet neu.
4. Wieder `F8` → Samsung-SSD. Erster Start von Beize: Es legt die Partitionen an,
   verschlüsselt sie und zeigt den **Wiederherstellungsschlüssel** auf dem
   Bildschirm – abschreiben bzw. in 1Password ablegen. Er wird nur dieses eine Mal
   angezeigt.
5. Prüfen: Beize startet ein zweites Mal ohne Eingabe; Windows startet (ohne `F8`)
   weiterhin mit Secure Boot.

**Notausgang:** BIOS → Key Management → „Load Default Secure Boot Keys“ stellt den
Werkszustand wieder her. Windows startet dann wie vorher; Beize startet nicht mehr
unter Secure Boot, seine Daten bleiben verschlossen, bis Schritt 3 wiederholt wird.

**Wenn Beize meldet „This start was changed“:** Es wurde etwas zum Start hinzugefügt
(Kernel-Parameter, Credentials oder Add-ons, z. B. über die Firmware). Beize öffnet
die Daten dann nicht und schaltet nach 60 Sekunden aus – **den
Wiederherstellungsschlüssel nicht eingeben**, sondern die Ursache klären.

**ASUS setzt bei BIOS-Updates die Schlüssel oft auf Werkszustand zurück.** Dann
startet Beize nicht unter Secure Boot (die Daten bleiben verschlossen) – Schritt 3
wiederholen.
