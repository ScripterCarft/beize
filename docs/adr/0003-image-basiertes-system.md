# 0003 – Unveränderliches, signiertes System-Image

Status: angenommen · 2026-10-06 · Aufbau der Partitionen präzisiert durch ADR 0007

## Kontext

Ziel ist ein System, das ohne Pflege des Nutzers läuft: keine Registry, kein
schleichender Verfall, nachvollziehbarer Speicherverbrauch, sichere Updates.

## Entscheidung

- Das System ist ein read-only Image auf zwei Partitionen (A/B) mit automatischem Rollback.
- Gestartet wird ein Unified Kernel Image (Kernel, Initramfs, Kommandozeile in
  einer signierten EFI-Datei) mit Secure Boot und eigenen Schlüsseln.
- Die Integrität des Systems wird mit dm-verity geprüft, `/home` ist mit LUKS und TPM2 verschlüsselt.
- Apps sind eigenständige Bundles außerhalb des System-Images.

## Konsequenzen

- Ein klassischer Paketmanager auf dem laufenden System entfällt.
- Das Build-Werkzeug erzeugt Images, keine Pakete.
- Ab M1 bootet bereits ein UKI auf einer GPT-Disk. Signatur, A/B und verity folgen in M2.
