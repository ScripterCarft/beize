# Secure-Boot-Schlüssel für echte Hardware

Hier liegen nur **öffentliche** Dateien der Produktionsschlüssel des Besitzers
(ADR 0010). Die privaten Teile liegen ausschließlich im 1Password-Tresor „Beize“
und existieren auf dem Build-Rechner nur im Arbeitsspeicher, während `bz` sie
braucht. Tests und CI benutzen eigene Wegwerf-Schlüssel und brauchen nichts von hier.

| Datei | Inhalt |
| --- | --- |
| `PK.crt`, `KEK.crt`, `db.crt` | Zertifikate von Plattform-, Schlüsselaustausch- und Signaturschlüssel |
| `pcr.pub.pem` | öffentlicher PCR-Schlüssel – steckt als `.pcrpkey` in jedem UKI |
| `update.pub.pem` | öffentlicher Update-Schlüssel (ML-DSA-65) – liegt in `/usr`, beize-updated nimmt nur damit signierte Updates an (ADR 0020) |
| `module.crt` | Zertifikat des Modul-Schlüssels (ML-DSA-87) – steckt im Kernel, der nur damit signierte Module lädt (ADR 0015) |
| `PK.auth`, `KEK.auth`, `db.auth`, `dbx.auth` | signierte Firmware-Updates, die systemd-boot einschreibt (Profil „Beize + Windows“: inklusive Microsofts Zertifikaten und Sperrliste) |

Alle Befehle unten führt **nur der Besitzer** aus, in einem eigenen Terminal
(WSL). `bz` verweigert sie ohne Terminal – Automatisierung und Claude kommen
nicht an die Schlüssel.

## Voraussetzung: 1Password-CLI

Unter WSL spricht nur die **Windows**-CLI mit der 1Password-App:
1. 1Password-CLI für Windows installieren (`winget install AgileBits.1Password.CLI`).
2. In der 1Password-App: Einstellungen → Entwickler → „Mit 1Password CLI integrieren“.
3. In WSL prüfen: `op.exe vault list` zeigt den Tresor „Beize“ (Windows Hello fragt).
4. Bei mehreren 1Password-Konten zeigt `bz` beim ersten Mal eine Auswahl (E-Mail,
   Adresse, entsperrt/gesperrt) und merkt sich die Wahl auf Wunsch in
   `~/.config/beize/1password-account` (löschen, um neu zu wählen). Alternativ
   `BZ_OP_ACCOUNT=<Konto>` setzen; das hat Vorrang.

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

## 1b. Update-Schlüssel erzeugen (einmalig, ADR 0020)

```sh
./bz secureboot new-update-key
git add keys/secureboot/update.pub.pem && git commit -m "Add the production update key (public part)"
```

Für Schlüssel aus der Zeit vor ADR 0020 (`new-keys` legt ihn inzwischen mit an). Der
private Teil liegt als „Beize update signing key“ im Tresor; ohne `update.pub.pem`
verweigert `bz sign` das Signieren.

## 1c. Modul-Schlüssel erzeugen (einmalig, ADR 0015)

```sh
./bz secureboot new-module-key
git add keys/secureboot/module.crt && git commit -m "Add the production module key (certificate)"
```

Für Schlüssel aus der Zeit, als jeder Kernel-Bau einen Wegwerf-Schlüssel erzeugte
(`new-keys` legt ihn inzwischen mit an). Der private Teil liegt als „Beize kernel
module signing key“ im Tresor; ohne `module.crt` verweigert `bz sign` das Signieren.

## 2. Image signieren (bei jeder Installation und jedem Update)

```sh
./bz build
./bz sign      # → build/out/desktop/production/disk.img
```

Braucht nur db-, PCR- und Modul-Schlüssel (1Password fragt per Windows Hello) und
den öffentlichen Update-Schlüssel von hier. `bz sign` prüft, dass sie zu den Dateien
hier passen, und löscht sie danach wieder aus dem Arbeitsspeicher; es linkt den Kernel
mit dem Modul-Zertifikat neu (Sekunden) und signiert die Module. Dieses Image ist für echte Hardware gebaut: Konsole auf dem
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

**Neu installieren** (ersetzt Beize und alles, was es auf der Platte gespeichert
hat): wie oben, aber mit der GUID, die Windows jetzt für die Platte anzeigt – Beizes
eigene `{6265697a-6500-4000-8000-000000000001}` – und `--reinstall`:
`./bz install /dev/sde --guid "{6265697a-6500-4000-8000-000000000001}" --reinstall`.
Ohne `--reinstall` lehnt `bz` eine Platte mit Beize ab, mit `--reinstall` jede ohne.

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
   angezeigt; der Start wartet, bis `stored` eingetippt ist.
5. Prüfen: Beize startet ein zweites Mal ohne Eingabe; Windows startet (ohne `F8`)
   weiterhin mit Secure Boot.

**Notausgang:** BIOS → Key Management → „Load Default Secure Boot Keys“ stellt den
Werkszustand wieder her. Windows startet dann wie vorher; Beize startet nicht mehr
unter Secure Boot, seine Daten bleiben verschlossen, bis Schritt 3 wiederholt wird.

**Wiederherstellung** (Passwort vergessen, Anmelden klappt nicht): Beim Start von
Beize – nach `F8` und der Samsung-SSD – die Leertaste gedrückt halten, bis das Menü
von systemd-boot erscheint, und „… (Recovery)“ wählen. Beize fragt nach dem
Wiederherstellungsschlüssel und zeigt dann „Beize recovery.“: `s` öffnet eine
Root-Shell mit den Daten, `r` startet neu. Der nächste normale Start entsperrt wieder
ohne Eingabe (ADR 0018).

**Wenn Beize meldet „This start was changed“:** Es wurde etwas zum Start hinzugefügt
(Kernel-Parameter, Credentials oder Add-ons, z. B. über die Firmware). Beize öffnet
die Daten dann nicht und schaltet nach 60 Sekunden aus – **den
Wiederherstellungsschlüssel nicht eingeben**, sondern die Ursache klären.

**ASUS setzt bei BIOS-Updates die Schlüssel oft auf Werkszustand zurück.** Dann
startet Beize nicht unter Secure Boot (die Daten bleiben verschlossen) – Schritt 3
wiederholen.

## 4. Uhr im Dual-Boot (einmalig, unter Windows)

Beize hält die Hardware-Uhr in UTC, Windows in Ortszeit – sonst geht eine der beiden
Uhren 1–2 Stunden falsch (ADR 0012). Windows auf UTC umstellen, in einer
Administrator-PowerShell:

    reg add "HKLM\SYSTEM\CurrentControlSet\Control\TimeZoneInformation" /v RealTimeIsUniversal /t REG_DWORD /d 1 /f

Dann Windows neu starten und unter Einstellungen → Zeit und Sprache → Datum und
Uhrzeit „Jetzt synchronisieren“. Rückgängig: derselbe Befehl mit `/d 0`.
