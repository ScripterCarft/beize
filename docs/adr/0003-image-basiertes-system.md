# 0003 – Unveränderliches, signiertes System-Image

Status: angenommen · 2026-10-06 · Endfassung 2026-10-10 (Aufbau im Einzelnen: ADR 0007–0010)

## Kontext

Ziel ist ein System, das ohne Pflege des Nutzers läuft: keine Registry, kein
schleichender Verfall, nachvollziehbarer Speicherverbrauch, sichere Updates.

## Entscheidung

- Das System (`/usr`) ist ein read-only Image in zwei Slots (A/B) mit automatischem
  Rückfall (ADR 0007); alles, was dem Besitzer gehört, liegt auf der Root-Partition.
- Gestartet wird ein Unified Kernel Image (Kernel, Initramfs, Kommandozeile in
  einer signierten EFI-Datei) mit Secure Boot und eigenen Schlüsseln (ADR 0006, 0010).
- Die Integrität von `/usr` prüft dm-verity (ADR 0008); die Root-Partition (`/etc`,
  `/var`, `/home`) ist mit LUKS2 verschlüsselt und an TPM2 gebunden (ADR 0009).
- Apps sind eigenständige Bundles außerhalb des System-Images.

## Konsequenzen

- Ein klassischer Paketmanager auf dem laufenden System entfällt.
- Das Build-Werkzeug erzeugt Images, keine Pakete.
- Updates schreiben den jeweils anderen Slot; dafür fehlt noch der Updater
  (systemd-sysupdate).
