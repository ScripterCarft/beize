# 0004 – Entwicklung in WSL2

Status: angenommen · 2026-10-06

## Kontext

Der Arbeitsrechner läuft mit Windows 11. Kernel- und Systembuilds brauchen ein
Linux-Dateisystem (Groß-/Kleinschreibung, Rechte, Geschwindigkeit) und KVM für
schnelle Testläufe.

## Entscheidung

- Werkbank ist Ubuntu 26.04 LTS in WSL2. Das Repository liegt im ext4-Dateisystem
  von WSL (`~/beize`), nicht auf einem Windows-Laufwerk.
- Testläufe erfolgen in QEMU mit KVM und OVMF (UEFI). Echte Hardware wird auf einer
  separaten SSD getestet.
- Bearbeitet wird mit Rider oder VS Code über deren WSL-Integration.

## Konsequenzen

- Builds laufen mit voller Geschwindigkeit, ein Kernel-Build dauert wenige Minuten.
- Später kann Beize sich selbst bauen. Das Build-Werkzeug ist dafür nicht an WSL gebunden.
