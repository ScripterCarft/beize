# 0009 – Root-Partition mit LUKS2, versiegelt an den TPM

Status: angenommen · 2026-10-07 · Endfassung 2026-10-10 (PCR 12 und
Bindung nur in der initrd: ADR 0010)

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
- **Bindung** (`TPM2PCRs=7+12` in `layout.rs`, Drop-in für systemd-repart in
  `system/initrd/`):
  - **PCR 7**, der Secure-Boot-Zustand: an/aus, PK/KEK/db/dbx und welches
    db-Zertifikat den Start freigegeben hat. Weil nur Beize-signierte Binaries
    starten und die Kommandozeile im signierten UKI steckt, heißt „PCR 7 stimmt“:
    Es läuft ein von uns signierter Start auf diesem Rechner.
  - **PCR 12**: Dem Start wurde nichts hinzugefügt (Kernel-Parameter über SMBIOS,
    Credentials, Add-ons; Begründung in ADR 0010).
  - **Signierte PCR-11-Richtlinie mit Policy-Referenz `initrd`**: repart übernimmt
    den öffentlichen PCR-Schlüssel aus dem UKI (`.pcrpkey`) von selbst. Nur die
    Signatur für die Phase „enter-initrd“ erfüllt die Richtlinie. Sobald die initrd
    „leave-initrd“ in PCR 11 misst, gibt der TPM den Schlüssel nicht noch einmal
    heraus, auch nicht dem laufenden System. Das ist eine zusätzliche Hürde, kein Schutz
    gegen ein kompromittiertes System: Nach dem Entsperren hält der Kernel den
    Datenschlüssel (dm-crypt), wie bei jeder Festplattenverschlüsselung.
- Beim Start findet systemd-gpt-auto-generator die Root-Partition und entsperrt sie
  mit `tpm2-device=auto`.
- **Recovery-Key, verpflichtend:** `beize-recovery-key.service` (initrd, direkt nach
  dem ersten Entsperren) erzeugt ihn mit `systemd-cryptenroll --recovery-key`
  (64 leicht abtippbare Zeichen) und zeigt ihn **genau einmal** an. Der Start wartet,
  bis der Besitzer `saved` eintippt (auf echter Hardware leerte die Anmeldung sonst
  den Bildschirm, bevor der Key zu sehen war). Bis zum Neustart liegt er nur im
  Arbeitsspeicher (`/run/beize/recovery-key`, nur root), nie auf der Platte oder im
  Journal. Solange der TPM entsperrt, lässt er sich ersetzen:
  `systemd-cryptenroll --unlock-tpm2-device=auto --wipe-slot=recovery --recovery-key`.
  Die initrd enthält dafür bash (keine coreutils, das Skript nutzt nur Builtins).
- **Signierte PCR-11-Richtlinie im UKI** (`.pcrsig` + `.pcrpkey`, ukify
  `--pcr-private-key … --sign-initrd-pcrs`). systemd 262 braucht sie zusätzlich, um
  die NvPCRs des TPM einzurichten (`systemd-tpm2-setup-early`). Signiert wird mit einem
  eigenen PCR-Schlüssel (RSA-2048): in Tests ein Wegwerf-Schlüssel, für echte Hardware
  der Produktionsschlüssel des Besitzers (ADR 0010).
- **ukify und systemd-measure kommen aus unserem eigenen systemd-Build**, nicht vom
  Build-Host: Sie müssen zur Version von Stub und systemd im Image passen. ukify liegt
  im Sysroot unter `usr/lib/beize-build/` (wird nie ausgeliefert), systemd-measure
  läuft über den Loader des Sysroots auf dem Host.
- Kernel: TPM-Treiber für CRB (Intel PTT, QEMU) und TIS (diskrete TPMs) fest
  eingebaut. `TCG_TPM2_HMAC` (Schutz gegen Mitlesen auf dem TPM-Bus) bleibt aus, weil
  der Zielrechner Intel PTT ohne externen Bus nutzt. TPM-Software: tpm2-tss (signiert,
  nur ESYS/MU/RC/TCTI-Device, ohne FAPI), von systemd per dlopen geladen.

## Verworfene Alternativen

- **PCR 0/2 (Firmware-Code) zusätzlich:** Jedes BIOS-Update würde das System aussperren.
- **TPM + PIN:** möglich, widerspricht aber dem Ziel „startet ohne Eingabe“. Kann
  später als Option kommen.
- **`systemd-cryptenroll --firstboot`** (systemd 262): Der Recovery-Key ist dort
  optional (Enter überspringt) und läuft über ein Textmenü. Beize verlangt den Key
  und zeigt ihn selbst an.
- `systemd-pcrlock`: siehe ADR 0010.

## Konsequenzen

- Schutz gegen: Ausbau der SSD, Start eines fremden Systems, abgeschaltetes oder
  verändertes Secure Boot, hinzugefügte Kernel-Parameter. Kein Schutz gegen jemanden,
  der den laufenden, entsperrten Rechner bedienen kann; das ist Aufgabe von Anmeldung
  und Sperrbildschirm.
- **Rollback:** Jedes ältere, mit demselben PCR-Schlüssel signierte UKI erfüllt die
  Richtlinie ebenfalls (Abhilfe erst mit Versions-Sperre oder Schlüsselwechsel).
- **Ändert sich PCR 7** (dbx-Update, BIOS-Update, neue Schlüssel) **oder ist es ein
  anderer TPM**, öffnet nur der Recovery-Key. `beize-tpm` bindet danach neu, aber nur
  einen erlaubten Zustand (ADR 0010).
- `bz smoke` prüft: `root=encrypted` bei jedem Start; im LUKS2-Header PCR 7+12 und die
  signierte Richtlinie über PCR 11 mit Referenz `initrd`; dass das laufende System den
  TPM-Schlüssel nicht bekommt (Boot-Report `systemunlock=refused`); den Recovery-Key
  beim ersten Start und nur dort; Gegentests mit einem anderen TPM und mit
  abgeschaltetem Secure Boot (jeweils auf einer Kopie der Installation, eigener swtpm).
- Kosten: rund 0,1 s Startzeit. Der erste Start dauert durch Verschlüsseln und
  Formatieren ~8 s.
