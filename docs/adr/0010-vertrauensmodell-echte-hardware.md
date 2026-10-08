# 0010 – Beize als Plattform-Eigentümer, Vertrauensprofile, Produktionsschlüssel

Status: angenommen · 2026-10-08 · ersetzt den Schlüsselteil von ADR 0006

## Kontext

ADR 0006 sah eigene PK/KEK/db **ohne** Microsoft-Zertifikate vor. Beize soll aber
beides perfekt können: **neben Windows** laufen (der Rechner des Besitzers bis M4)
und **als einziges System**. Mit „nur Beize“ auf einem Rechner mit Windows:

- Windows startet nicht mehr mit Secure Boot (Anti-Cheat, Windows 11).
- Option-ROMs (Grafikkarte, Netzwerk) sind von Microsoft signiert – ohne diese
  Zertifikate bleibt der Bildschirm beim Start im schlimmsten Fall schwarz.
- Windows kann keine Secure-Boot-Updates (dbx, Zertifikatswechsel 2011 → 2023)
  einspielen; sie sind mit Microsofts KEK signiert.

Kurz erwogen und verworfen: Werksschlüssel behalten und Beizes db-Zertifikat im
BIOS-Menü ergänzen. Sicher, aber ein Handgriff im BIOS, den kein Test prüft,
Beize könnte die Schlüssel nie selbst verwalten, und für „nur Beize“ bräuchte es
einen zweiten Weg.

## Entscheidung

**Beize ist Eigentümer der Plattform** (eigener PK und KEK), andere Hersteller
sind über ein **Vertrauensprofil** zusätzlich vertraut – das Modell von `sbctl`
und systemd-boot:

| | Profil „Beize + Windows“ (jetzt) | Profil „Nur Beize“ (später) |
| --- | --- | --- |
| PK | Beize | Beize |
| KEK | Beize + Microsoft KEK 2011/2023 | Beize |
| db | Beize + Microsoft (Windows-Bootmanager 2011/2023, UEFI CA 2011/2023, Option ROM CA 2023) | Beize + was die Option-ROMs des Rechners brauchen |
| dbx | Microsofts aktuelle Sperrliste | dieselbe |

- Microsofts Objekte liegen mit fester Prüfsumme in `keys/microsoft/` (Quelle:
  `microsoft/secureboot_objects`, gegen Ubuntus OVMF gegengeprüft).
- **Einschreiben:** Jedes Image trägt signierte Variablen-Updates (`PK.auth`,
  `KEK.auth`, `db.auth`, `dbx.auth`) unter `/loader/keys/auto/` auf der ESP;
  `loader.conf` hat `secure-boot-enroll force`. Ist die Firmware im Setup-Modus
  (Besitzer hat im BIOS die Secure-Boot-Schlüssel gelöscht), schreibt systemd-boot
  nach 15 s Warnung (Taste bricht ab) alles ein und startet neu. Danach ist das
  ein No-op. PK signiert PK und KEK, KEK signiert db; dbx ist Microsofts eigenes
  signiertes Update.
- **Profilwechsel** (z. B. „Windows entfernen“) braucht kein BIOS: Beize besitzt
  PK/KEK und kann KEK/db selbst neu schreiben. Das Profil „Nur Beize“ kommt, wenn
  es gebraucht wird – vorher muss geklärt sein, welche Option-ROMs der Rechner hat
  (Hashes aus dem TPM-Ereignisprotokoll statt Microsofts UEFI CA).

**Schlüssel:** vier Produktionsschlüssel, alle RSA-2048 (von jeder Firmware
akzeptiert):

| Schlüssel | Zweck | gebraucht von |
| --- | --- | --- |
| PK | Plattform-Eigentümer, signiert PK- und KEK-Update | `bz secureboot new-keys` (später Profilwechsel) |
| KEK | signiert db-Updates | dito |
| db | signiert systemd-boot und UKI | `bz sign`, bei jedem Image |
| PCR | signiert die erwarteten PCR-11-Werte (`.pcrsig`) | `bz sign`, bei jedem Image |

- Private Teile **nur** im 1Password-Tresor „Beize“ (als Dokumente); öffentliche
  Teile und die signierten `.auth`-Dateien in `keys/secureboot/` (werden committet).
- `bz secureboot new-keys` erzeugt alles **nur im Arbeitsspeicher** (`/dev/shm`),
  legt die privaten Teile per 1Password-CLI ab, liest sie zur Kontrolle zurück und
  bricht ab, wenn es schon Schlüssel gibt (Schlüsselwechsel ist ein eigener Schritt).
- `bz sign` holt nur db und PCR, prüft, dass sie zu den Zertifikaten im Repo
  passen, baut nach `build/out/production/` und löscht sie wieder. PK und KEK
  bleiben im Alltag unberührt – wie Wurzelschlüssel.
- 1Password-CLI: unter WSL die Windows-CLI `op.exe` (nur sie spricht mit der
  Desktop-App, Freigabe per Windows Hello); `BZ_OP` kann ein anderes Programm setzen.
- **Beide Befehle laufen nur in einem echten Terminal** (stdin ist ein TTY) –
  technische Absicherung, dass Automatisierung (CI, Claude) nie an die
  Produktionsschlüssel kommt.

**Daten (TPM):** Der TPM-Slot der Root-Partition ist an PCR 7 (Secure-Boot-Zustand)
**und** an die signierte PCR-11-Richtlinie mit Policy-Referenz `initrd` gebunden:
Unsere eigenen Updates brauchen nie eine Eingabe, und nur die initrd eines signierten
Beize-UKI bekommt den TPM-Schlüssel – das laufende System nicht noch einmal (eine Hürde,
kein Schutz gegen ein kompromittiertes System: nach dem Entsperren hält der Kernel den
Datenschlüssel). PCR 7 enthält auch,
*welches* db-Zertifikat den Start freigegeben hat: Ein anderes, von Microsoft
signiertes System startet, bekommt den Schlüssel aber nicht.

**Nach einer Änderung neu binden – aber nur einen erlaubten Zustand:** Weigert sich
der TPM (Windows-Secure-Boot-Update, BIOS-Update, neue Schlüssel, neues Mainboard),
fragt systemd-cryptsetup nach dem Recovery-Key. Danach entscheidet `beize-tpm`
(eigener Rust-Code, `os/beize-tpm`, `beize-tpm.service` in der initrd), ob der neue
Zustand abgesegnet werden darf – der Recovery-Key allein genügt nicht:
- **Secure Boot an** (EFI-Variable und Messung).
- **Gemessener Zustand passt zum Profil:** Das Firmware-Ereignisprotokoll muss,
  nachgerechnet, exakt PCR 7 des TPM ergeben (plus genau einmal systemd's
  `os-separator`). PK ist exakt Beizes; KEK und db enthalten nur Einträge aus dem
  Profil (weniger ist erlaubt); jedes freigebende Zertifikat stammt aus db, eines davon
  Beizes eigenes; sonst nichts in PCR 7 (z. B. kein Firmware-Debug-Modus). dbx darf
  sich frei ändern. Das Profil (`PK.esl`, `KEK.esl`, `db.esl`, `beize-db.esl`) erzeugt
  `bz` beim Einschreiben und legt es als zweites initrd-Archiv **signiert ins UKI**.
- **Neuer TPM** (SRK anders als in `/var/lib/beize/tpm-srk.pem`; systemd's eigene
  Kopie wird bei TPM-Wechsel überschrieben): nur nach ausdrücklicher Bestätigung
  („yes“) auf der Konsole – ein getauschter Chip wird nie stillschweigend übernommen.
Erst dann löscht `beize-tpm` den alten TPM-Slot (sonst hält `systemd-cryptenroll`
den Slot eines anderen TPM mit gleichen PCR-Werten für aktuell) und schreibt neu ein:
PCR 7 + signierte PCR-11-Richtlinie „initrd“. Den eingegebenen Schlüssel liest es aus
dem Zwischenspeicher von systemd-cryptsetup im Kernel-Schlüsselbund
(`KeyringMode=shared`) und gibt ihn nur über stdin an `systemd-cryptenroll` weiter.
Sonst: Ablehnung mit Grund auf der Konsole, der Recovery-Key bleibt nötig.
Grenzen: Ein neuer TPM wird nicht über sein Endorsement-Zertifikat geprüft
(Attestierung), sondern durch die bewusste Bestätigung des Besitzers.

**Verworfen: `systemd-pcrlock`.** Es schützt seine NV-Richtlinie mit einer eigenen
PIN, die an dieselbe Richtlinie gebunden ist; ändert sich PCR 7 *unvorhergesehen*
(genau der Windows-Fall), braucht es zusätzlich eine zweite Recovery-PIN. Dazu
NV-Index-Verwaltung, Credential auf der ESP und „experimentell“ in v262; einen
neuen TPM deckt es nicht ab. Wieder prüfen, wenn Beize selbst Firmware-/dbx-Updates
einspielt (dann lassen sich Änderungen vorhersagen). Die pcrlock-Dienste sind per
Preset aus.

**Tests:** Jede Test-VM startet mit Firmware im Setup-Modus; systemd-boot schreibt
die Schlüssel ein, `bz smoke` prüft danach PK/KEK/db/dbx in der Firmware und erst
dann die Starts – der Weg ist derselbe wie auf echter Hardware. Zusätzlich
durchläuft ein Test den Ablauf des Besitzers (`new-keys` → `sign`) mit einem
Wegwerf-Schlüsselspeicher statt 1Password und bootet das Ergebnis.

## Konsequenzen

- Einmaliger Schritt im BIOS: Secure-Boot-Schlüssel löschen (Setup-Modus). Ein
  BIOS-Passwort ist optional (wie bei Windows): Wer ohne es Secure Boot abschaltet
  oder Schlüssel ändert, verändert PCR 7, und die Daten bleiben zu. Es schützt
  zusätzlich gegen Manipulation, wenn Fremde Zugang zum Rechner haben.
- **ASUS setzt bei BIOS-Updates die Schlüssel oft auf Werkszustand zurück.** Dann
  startet Beize nicht unter Secure Boot (Daten bleiben zu) – Schlüssel erneut
  löschen, systemd-boot schreibt beim nächsten Start wieder ein.
- PCR 7 ändert sich bei dbx-/db-Updates durch Windows und bei BIOS-Updates
  (erfahrungsgemäß wenige Male pro Jahr); bis pcrlock umgesetzt ist, verlangt
  Beize dann den Recovery-Key.
- Beize trägt als PK-Eigentümer die Verantwortung, die Microsoft sonst der
  Hersteller trägt: neue Microsoft-KEKs (wie 2023) kommen nur über uns in die
  Firmware – `keys/microsoft/` aktuell halten.
