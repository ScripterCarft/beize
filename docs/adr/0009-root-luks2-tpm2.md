# 0009 – Root-Partition mit LUKS2, versiegelt an TPM2 und PCR 7

Status: angenommen · 2026-10-07

## Kontext

`/usr` ist signiert und unveränderlich (ADR 0007/0008), aber die Daten des
Besitzers – `/etc`, `/var`, `/home` auf der Root-Partition – lagen im Klartext auf
der Disk. Wer die SSD ausbaut oder ein anderes System startet, konnte alles lesen.
Ziel: Verschlüsselung ohne Eingabe beim Start, aber nur auf genau diesem Rechner
mit intaktem Secure Boot.

## Entscheidung

- systemd-repart legt die Root-Partition beim ersten Start in der initrd als
  **LUKS2-Volume** an (`Encrypt=tpm2`, aes-xts-plain64 über dm-crypt mit AES-NI)
  und versiegelt den Schlüssel im TPM. Es gibt keinen Passwort-Slot.
- Bindung an **PCR 7** (`TPM2PCRs=7`): Dort misst die Firmware den Secure-Boot-
  Zustand – an/aus, PK/KEK/db/dbx und welches db-Zertifikat den Start
  freigegeben hat. Weil nur Beize-signierte Binaries starten und die
  Kommandozeile im signierten UKI steckt, heißt „PCR 7 stimmt“: Es läuft
  ein von uns signierter Start auf diesem Rechner.
- Beim Start findet systemd-gpt-auto-generator die verschlüsselte Root-Partition
  und entsperrt sie mit `tpm2-device=auto`; der Volume-Schlüssel wird zusätzlich
  in einen PCR gemessen (systemd-Standard).
- Kernel: TPM-Treiber für CRB (Intel PTT, QEMU) und TIS (diskrete TPMs) fest
  eingebaut; `TCG_TPM2_HMAC` (Schutz gegen Mitlesen auf dem TPM-Bus) bleibt
  vorerst aus – der Zielrechner nutzt Intel PTT ohne externen Bus.
- TPM-Software: tpm2-tss (signiert, nur ESYS/MU/RC/TCTI-Device, ohne FAPI),
  von systemd per dlopen geladen – im System und in der initrd.
- **Signierte PCR-11-Richtlinie im UKI** (`.pcrsig` + `.pcrpkey`, ukify
  `--pcr-private-key … --sign-initrd-pcrs`): systemd 262 richtet damit beim Start
  die NvPCRs des TPM ein (`systemd-tpm2-setup-early`); ohne sie schlägt der Dienst
  fehl. Signiert wird mit einem eigenen PCR-Schlüssel (RSA-2048), in Tests ein
  Wegwerf-Schlüssel neben den Secure-Boot-Testschlüsseln, für echte Hardware ein
  Produktionsschlüssel des Besitzers (wie ADR 0006).
- **ukify und systemd-measure kommen aus unserem eigenen systemd-Build**, nicht vom
  Build-Host: Sie müssen zur Version von Stub und systemd im Image passen (ukify 259
  des Hosts kennt die initrd-Richtlinien von 262 nicht). ukify liegt im Sysroot
  unter `usr/lib/beize-build/` (wird nie ausgeliefert); systemd-measure läuft über
  den Loader des Sysroots auf dem Host.

## Verworfene Alternativen

- **PCR 0/2 (Firmware-Code) zusätzlich:** jedes BIOS-Update sperrt das System aus.
- **Root zusätzlich an die signierte PCR-11-Richtlinie binden** (bindet an genau
  unsere UKIs, übersteht Updates; mit Policy-Referenz `initrd` ist der Schlüssel
  sogar nur in der initrd entsperrbar, nicht mehr aus dem laufenden System):
  die Signatur ist jetzt vorhanden, die Bindung kommt aber erst mit dem
  Produktionsschlüssel-Workflow – sonst hinge die Root-Partition an einem
  Testschlüssel.
- **TPM + PIN:** möglich, widerspricht aber dem Ziel „startet ohne Eingabe“; kann
  später als Option kommen.

## Konsequenzen

- Schutz gegen: Ausbau der SSD, Start eines fremden Systems, abgeschaltetes oder
  verändertes Secure Boot. Kein Schutz gegen jemanden, der den laufenden,
  entsperrten Rechner bedienen kann – das ist Aufgabe von Anmeldung und Sperrbildschirm.
- Schwäche von PCR 7 allein: Jeder ältere, mit demselben db-Schlüssel signierte
  UKI entsperrt ebenfalls (Rollback). Abhilfe kommt mit der PCR-11-Richtlinie
  bzw. dbx-Einträgen.
- **Ändert sich PCR 7** (Firmware-Update setzt Schlüssel zurück, Schlüsselwechsel,
  dbx-Update) **oder stirbt der TPM**, öffnet nur noch der Recovery-Key die Daten
  (siehe unten).
- `bz smoke` verlangt `root=encrypted` bei jedem Start und prüft die Gegenrichtung:
  dieselbe Disk mit einem anderen TPM und mit abgeschaltetem Secure Boot darf
  nicht entsperren. Jede VM hat dafür ihren eigenen Software-TPM (swtpm).
- Kosten: rund 0,1 s Startzeit (gemessen 5,23 s statt ~5,1 s Gast-Uptime); der
  erste Start dauert durch Verschlüsseln und Formatieren ~7 s.

## Nachtrag: Recovery-Key (2026-10-07)

- Jede Installation bekommt beim ersten Start **verpflichtend** einen Recovery-Key
  (`beize-recovery-key.service` → `systemd-cryptenroll --unlock-tpm2-device=auto
  --recovery-key`): zufällig erzeugt, 64 leicht abtippbare Zeichen.
- Er wird **genau einmal angezeigt** (Konsole, später im Onboarding) und liegt bis
  zum nächsten Neustart nur im Arbeitsspeicher (`/run/beize/recovery-key`, nur root).
  Er wird nie auf die Platte und nie ins Journal geschrieben; aufbewahren muss ihn
  der Besitzer selbst (Claude greift nie auf 1Password zu).
- Solange der TPM noch entsperrt, lässt sich ein verlorener Key ersetzen:
  `systemd-cryptenroll --unlock-tpm2-device=auto --wipe-slot=recovery --recovery-key`.
- Verworfen: der Assistent `systemd-cryptenroll --firstboot` von systemd 262 –
  optional (Enter überspringt), textbasiertes Menü in der initrd; Beize verlangt den
  Key und zeigt ihn selbst an.
- `bz smoke` prüft: Key beim ersten Start angezeigt, beim zweiten nicht mehr; mit
  einem anderen TPM fragt das System nach dem Key, und mit ihm startet es normal.

## Nachtrag: Bindung nur in der initrd (2026-10-08, ADR 0010)

- Gemessen am LUKS2-Header: repart band den TPM-Slot schon bisher **zusätzlich** an
  die signierte PCR-11-Richtlinie (`tpm2_pubkey_pcrs: [11]`), weil es den
  öffentlichen PCR-Schlüssel aus dem UKI (`/run/systemd/tpm2-pcr-public-key.pem`)
  von selbst verwendet. Die Annahme oben („erst mit Produktionsschlüsseln“) war falsch.
- Jetzt zusätzlich mit **Policy-Referenz `initrd`** (`systemd-repart.service`-Drop-in
  in `system/initrd/`): Nur die Signatur, die das UKI für die Phase „enter-initrd“
  trägt, erfüllt die Richtlinie. Sobald die initrd „leave-initrd“ in PCR 11 misst,
  gibt der TPM den TPM-Schlüssel nicht **noch einmal** heraus. Das ist eine zusätzliche
  Hürde, kein Schutz gegen ein kompromittiertes laufendes System: Nach dem Entsperren
  hält der Kernel den Datenschlüssel (dm-crypt), wer dort root ist, liest die Daten und
  kann den Schlüssel aus dem Speicher holen – wie bei jeder Festplattenverschlüsselung.
  Der Boot-Report prüft einmal pro Start, dass der TPM nach der initrd verweigert
  (`systemunlock=refused`); der TPM überwacht das laufende System nicht.
- Der Recovery-Key wird deshalb **in der initrd** angelegt
  (`beize-recovery-key.service` in `system/initrd/`), direkt nach dem Entsperren.
  Die initrd enthält dafür bash (keine coreutils; das Skript nutzt nur Builtins).
  stderr von `systemd-cryptenroll` (im interaktiven Modus mit QR-Code des Schlüssels)
  erscheint nur im Fehlerfall und nie im Journal.
- Verbleibende Schwäche: Jedes ältere, mit demselben PCR-Schlüssel signierte UKI
  erfüllt die Richtlinie ebenfalls (Rollback). PCR 7 bleibt direkt gebunden; ändert es
  sich (dbx-Update, BIOS-Update), bindet `beize-tpm-reseal` nach einmaliger Eingabe
  des Recovery-Keys neu (ADR 0010).
