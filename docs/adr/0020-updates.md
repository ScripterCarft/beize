# 0020 – Updates: A/B mit Beizes Updater und systemd-sysupdate

Status: angenommen · 2026-10-11 (Entscheidungen mit dem Besitzer abgestimmt)

## Kontext

ADR 0007 hat den Platz vorbereitet: zwei `/usr`-Slots, UKIs mit Boot-Zähler, Root mit allen
Daten des Besitzers. Es fehlte, wer ein Update holt, prüft und schreibt. Bisher heißt jede neue
Version Neuinstallation mit neuem Recovery-Key.

Das Werkzeug dafür bringt systemd mit: `systemd-sysupdate` schreibt ein Update in den freien
Slot (Partition mit Label `_empty` oder der älteren Version), setzt die Labels, legt das UKI
mit Zähler ab und räumt alte Versionen weg. Seine Prüfung heruntergeladener Dateien gibt es
aber nur über GnuPG: `SHA256SUMS.gpg`, ausgewertet von `gpg` als root; Dateien aus lokalen
Quellen prüft es gar nicht. Auf die Prüfung verzichten geht nicht: Secure Boot und dm-verity
lassen zwar nur von uns signierte Systeme starten, aber ein Angreifer könnte ein **altes**,
korrekt signiertes UKI unter einem höheren Versionsnamen unterschieben (Downgrade auf eine
Version mit bekannter Lücke), oder Datenmüll in den Slot schreiben lassen.

## Entscheidung

- **Beize ist die Instanz: `beize-updated`** (`os/beize-update`), wie `softwareupdated` auf
  dem Mac und `update_engine` unter ChromeOS. Er entscheidet, was wann installiert wird;
  `beize-update` ist sein Client (später die Einstellungen, M5). systemd-sysupdate ist das
  Werkzeug darunter, nicht die Schnittstelle (`sysupdated`/`updatectl` werden nicht gebaut).
- **Ablauf:**
  1. *Holen* – unprivilegiert (`DynamicUser`, nur Netz und ein Cache-Verzeichnis): Liste
     `SHA256SUMS` und ihre Signatur `SHA256SUMS.sig`, dann die Dateien der neuesten Version.
  2. *Prüfen* – Signatur mit OpenSSL (libcrypto, schon im Image) gegen den öffentlichen
     Update-Schlüssel in `/usr`; die Version muss höher sein als die laufende; jede Datei
     muss ihrer SHA-256-Summe entsprechen. Erst dann sieht der privilegierte Teil sie.
  3. *Schreiben* – `systemd-sysupdate` mit Beizes Definitionen in `/usr/lib/sysupdate.d`
     und dem geprüften Verzeichnis als lokaler Quelle: `/usr`-Daten und verity-Baum in den
     freien Slot (Label `beize_<ver>`), danach das UKI als `beize_<ver>+3.efi`. Das UKI
     kommt zuletzt – ohne UKI wird kein halb geschriebener Slot gestartet.
- **Ein eigener Update-Schlüssel (ML-DSA-65)**, wie die Modulsignatur post-quanten-fest
  (ADR 0015), im 1Password-Tresor des Besitzers; nur `bz release` (Besitzer) signiert. Eine
  Schlüsselart, eine Bibliothek: kein GnuPG im Image, keine OpenPGP-Pakete von Servern in
  einem Prozess mit Rechten.
- **Quelle:** ein Verzeichnis oder eine URL (`file://`, `http(s)://`), z. B. ein USB-Stick
  oder ein Rechner im Heimnetz; die Signatur macht den Weg dorthin unerheblich. Ein
  öffentlicher Server wird vor M5 entschieden – dann prüft der Updater täglich selbst.
  Bis dahin startet der Besitzer ein Update: `run0 beize-update install <Quelle>`.
- **Ein Update wird beim nächsten Neustart aktiv**, nie durch einen erzwungenen Neustart
  (Vorbild ChromeOS: „Neu starten, um zu aktualisieren“).
- **Wann ein Start gelungen ist:** `beize-boot-health.service` vor `boot-complete.target`
  prüft, dass keine Unit aus `images/running-units.txt` fehlgeschlagen ist – dieselbe Liste,
  die der Smoke-Test verlangt, mit dem Updater darin: Die neue Version muss sich selbst
  wieder aktualisieren können. Erst danach entfernt `systemd-bless-boot` den Zähler; nach
  drei gescheiterten Starts wählt systemd-boot die vorige Version. Nicht jede fehlgeschlagene
  Unit (`systemd-boot-check-no-failures`): Ein Fehler, den auch die alte Version hätte (z. B.
  Hardware), würde sonst jedes künftige Update zurückrollen.
- **Bootloader:** Das signierte systemd-boot liegt als `systemd-bootx64.efi.signed` in `/usr`;
  `systemd-boot-update.service` (wieder an, ADR 0013) kopiert es, aber erst nach
  `boot-complete.target` – ein neuer Bootloader nur von einer bewährten Version. bootctl
  ersetzt ihn atomar und nur durch eine neuere Version.
- **Slot-Größe 4 GiB** je `/usr`-Slot (verity-Slot bleibt 64 MiB, der Baum braucht ~32 MiB).
  Heute 86 MB; Compositor, Mesa, NVIDIA-Firmware, Schriften und Audio kommen dazu. Vorbild
  ChromeOS: von 2 auf 4 GiB vergrößert, weil ältere Geräte Updates nicht mehr aufnehmen
  konnten. Die Größe gilt ab der nächsten Neuinstallation – der letzten.
- **Release:** `bz release` (Besitzer, mit 1Password) signiert Bootloader und UKI mit den
  Produktionsschlüsseln, baut daraus `/usr`, schreibt `build/release/<ver>/` mit
  `beize_<ver>.usr.erofs`, `beize_<ver>.usr.verity`, `beize_<ver>.efi`, `SHA256SUMS` und
  `SHA256SUMS.sig`. Die Version steht in `layout.rs` und steigt mit jedem Release.

## Konsequenzen

- Neue Pakete: xz und curl (systemd baut sysupdate nur mit importd); importd-Programme, die
  Beize nicht nutzt, löscht das Rezept mit Begründung.
- Das TPM bleibt ohne Eingabe: Das neue UKI erfüllt dieselbe signierte PCR-11-Richtlinie,
  PCR 7 ändert sich nicht. Weiter offen (ADR 0009): Jedes ältere signierte UKI tut das auch.
- Eine zurückgespielte alte Liste kann ein Update nur verhindern, nicht ein altes System
  installieren.
- systemds Benachrichtigung an `systemd-pcrlock` nach einem Update ist aus: pcrlock legte ein
  Credential auf die ESP, das systemd-stub in PCR 12 misst – der nächste Start galt als verändert
  und die Daten blieben zu (vom Update-Test gefunden). Beize versiegelt mit der signierten
  PCR-11-Richtlinie, nicht mit pcrlock.
- `/etc` bleibt beim Update unberührt; neue Vorgaben gehören nach `/usr` (systemds
  Vorrangregeln), nicht nach `/usr/share/factory/etc`.
- Image und `bz install` schreiben einen 4-GiB-Slot (das Image bleibt sparse).

## Abnahme

Smoke-Test: Update A → B aus einem Test-Release mit Test-Schlüsseln, Start ohne Recovery-Key,
Zähler entfernt; zweites Update B → A (überschreibt die älteste Version); kaputtes Update
(verity passt nicht) → nach drei Versuchen wieder die vorige Version; Gegenproben: veränderte
Liste, falsche Signatur, ältere Version und veränderte Datei werden abgelehnt, nichts wird
geschrieben.
