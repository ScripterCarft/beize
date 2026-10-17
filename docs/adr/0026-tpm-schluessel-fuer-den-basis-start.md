# 0026 – TPM-Schlüssel für den Basis-Start

Status: Entwurf · 2026-10-17 (zur Prüfung durch den Besitzer, noch nicht umgesetzt)

## Kontext

Der Desktop hat drei Starts im Menü: Beize, „(Recovery)“ und „(Basis)“ (ADR 0025). „(Basis)“
ist dasselbe signierte UKI mit `systemd.unit=multi-user.target`. Heute öffnet dort nur der
Recovery-Key die Daten, denn der TPM-Schlüssel der Daten (ADR 0009/0010) ist an drei Dinge
gebunden:

- **PCR 7:** Secure-Boot-Zustand (PK, KEK, db, dbx, wer die Startdateien signiert hat).
- **PCR 12** mit dem Wert eines normalen Starts: Nichts wurde hinzugefügt (Kernel-Parameter,
  Credentials, Add-ons). Wählt systemd-boot ein anderes Profil, misst es `@<Nummer>` und
  systemd-stub die Nummer. Damit ändert sich PCR 12.
- **PCR 11** über eine **signierte Richtlinie** (Schlüssel von Beize, Referenz `initrd`):
  nur ein von Beize signiertes UKI-Profil in der initrd. Signiert sind Beize und „(Basis)“,
  die Recovery nicht.

Ziel: „(Basis)“ soll wie ein normaler Start ohne Recovery-Key öffnen, ohne dass
irgendein anderer Start das kann.

Vorbild: Windows öffnet BitLocker im abgesicherten Modus ohne Recovery-Key, denn derselbe
signierte Bootmanager läuft. macOS startet den abgesicherten Modus mit dem
normalen FileVault-Kennwort. systemd sieht für mehrere erlaubte Startarten systemd-pcrlock
vor. Das wäre ein Umbau des ganzen TPM-Modells und bringt für diesen einen Fall nichts
Zusätzliches.

## Entscheidung (Vorschlag)

### 1. Die PCR-Richtlinie

Ein **zweiter TPM2-Schlüssel** (zweites Token in LUKS2), gebunden an:

| PCR | Hauptschlüssel (wie heute) | Basis-Schlüssel (neu) |
|---|---|---|
| 7 | aktueller Wert | derselbe aktuelle Wert |
| 12 | aktueller Wert eines normalen Starts (`0…0`) | **berechneter** Wert von Profil 2: `extend(extend(0, SHA256("@2" UTF-16LE+NUL)), SHA256("2" UTF-16LE+NUL))` |
| 11 | signierte Richtlinie, Referenz `initrd` | dieselbe signierte Richtlinie |

`beize-tpm` berechnet den Wert mit derselben Funktion, mit der es Profil 2 schon heute
erkennt (`policy::profile_pcr12`). `systemd-cryptenroll --tpm2-pcrs=7+12:sha256=<wert>`
nimmt ihn als erwarteten Wert. Er wird also nicht aus einem laufenden Start abgelesen.

Warum das kein anderer Start nutzen kann:

- **PCR 12** ist nur dann gleich, wenn genau Profil 2 gewählt und nichts hinzugefügt wurde.
  Ein zusätzlicher Kernel-Parameter oder ein Credential verändert ihn.
- **PCR 11** verlangt zusätzlich, dass das gewählte Profil eines ist, das Beize signiert
  hat. Wer Profil 2 eines fremden UKI wählt, scheitert an PCR 11 (nicht signiert) und an
  PCR 7 (nicht von Beizes Schlüsseln signiert).
- **Festlegung:** Profil 2 ist für immer „(Basis)“. Ein Test in `bz` (Reihenfolge der
  Profile) und die Konstante in `beize-tpm` halten das fest. Ein neues Profil bekommt
  Nummer 3 oder höher und keinen Schlüssel.
- Nach dem Entsperren läuft dasselbe System wie im normalen Start, nur ohne Desktop.
  Anmelden geht nur mit dem Passwort, es gibt keine Shell ohne Anmeldung. Der Basis-Start
  gibt also nicht mehr frei als ein normaler Start.

### 2. Binden und Neubinden

Heute bindet `beize-tpm reseal` in einem normalen Start, der mit dem Recovery-Key geöffnet
wurde (Secure-Boot-Update, neues Mainboard): erst `--wipe-slot=tpm2`, dann den neuen
Schlüssel anlegen. Neu:

1. Erster Start (systemd-repart): Hauptschlüssel wie heute. Danach legt `beize-tpm` den
   Basis-Schlüssel an, mit dem beim ersten Start erzeugten Recovery-Key. Ob er dort ohne
   neue Ablage erreichbar ist, prüfe ich bei der Umsetzung, bevor Code entsteht. Ist er
   es nicht, entsteht der Basis-Schlüssel beim ersten Basis-Start mit dem Recovery-Key
   (Schritt 3), einmal je Rechner.
2. Neubinden im normalen Start: alle TPM2-Schlüssel löschen, Hauptschlüssel anlegen,
   Basis-Schlüssel anlegen. Das geschieht in dieser Reihenfolge, damit der normale Start
   immer zuerst wieder funktioniert.
3. **Selbstheilung im Basis-Start:** Wurde „(Basis)“ mit dem Recovery-Key geöffnet und
   bestehen die Prüfungen wie beim Neubinden (Secure Boot an, PCR-7-Ereignisse im Profil,
   gleicher TPM), dann legt `beize-tpm` nur den fehlenden Basis-Schlüssel an. Den
   Hauptschlüssel rührt es dabei nicht an.

### 3. Wiederherstellung, wenn Binden misslingt

Der Recovery-Key-Slot wird nie angefasst. Jeder Zwischenzustand ist damit sicher und
heilt sich selbst:

| Abbruch nach | Normaler Start | Basis-Start | Heilung |
|---|---|---|---|
| Löschen der TPM-Schlüssel | Recovery-Key | Recovery-Key | nächster normaler Start mit Key: Schritt 2 |
| Hauptschlüssel angelegt | TPM | Recovery-Key | nächster Basis-Start mit Key: Schritt 3 |
| beide angelegt | TPM | TPM | – |

Ein Stromausfall mitten in `systemd-cryptenroll` ist ein Fall für LUKS2 selbst: Es schreibt
Metadaten mit doppeltem Header und Prüfsumme, also bleibt der alte oder der neue Zustand.
Der Smoke-Test prüft ohnehin, dass der Recovery-Key in jedem Fall öffnet.

## Prüfung

- **Unit-Tests (`beize-tpm`):** der berechnete PCR-12-Wert von Profil 2 gegen einen in
  QEMU gemessenen Wert; die Entscheidung von reseal für alle Fälle (normal, Basis mit und
  ohne vorhandenen Basis-Schlüssel, Recovery, verändert, Secure Boot aus).
- **Smoke-Test (volle Stufe):**
  - „(Basis)“ entsperrt per TPM, ohne Recovery-Key; es laufen genau die Units der Basis.
  - Profil 2 mit einem zusätzlichen Kernel-Parameter (SMBIOS-Credential) wird gestoppt wie
    heute und entsperrt nicht.
  - Die Recovery entsperrt weiterhin nicht per TPM.
  - Nach einem Secure-Boot-Update (Szenario gibt es schon) entsperren normaler Start
    **und** „(Basis)“ wieder per TPM.
  - Neues Mainboard (gibt es schon): ohne „yes“ entsperren beide nicht, mit „yes“ beide.
  - Ein fehlender Basis-Schlüssel (Token im Test entfernt): „(Basis)“ fragt einmal nach dem
    Recovery-Key, legt den Schlüssel an, der nächste Basis-Start entsperrt per TPM. Der
    normale Start bleibt die ganze Zeit per TPM.
  - Nach jedem Szenario genau zwei TPM2-Token mit den PCRs aus der Tabelle (die
    Prüfung `tpm2-pcrs == [7, 12]` in `vm/checks.rs` wird erweitert).

## Konsequenzen

- „(Basis)“ ist bequem wie der abgesicherte Modus von Windows und macOS, ohne dass ein
  veränderter Start die Daten öffnet.
- Ein Schlüssel mehr im Header, zwei Aufrufe von `systemd-cryptenroll` mehr beim Neubinden
  (einige Sekunden, selten).
- `beize-tpm reseal` wird umfangreicher, im sicherheitskritischsten Teil. Deshalb gibt es
  die Tests oben, bevor es auf einen echten Rechner kommt.
