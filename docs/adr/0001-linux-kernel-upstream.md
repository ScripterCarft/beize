# 0001 – Linux-Kernel aus Upstream

Status: angenommen · 2026-10-06

## Kontext

Beize soll ein eigenständiges OS sein. Ein eigener Kernel wäre denkbar, aber der
Großteil eines Kernels sind Treiber (GPU, WLAN, NVMe, USB, Energieverwaltung),
und genau die entscheiden über Stabilität und Akkulaufzeit.

## Entscheidung

Beize nutzt den offiziellen Linux-Kernel, gebaut aus dem Quellcode mit eigener
Konfiguration. Eigene Patches nur, wenn ein konkretes Problem es verlangt.
Gefolgt wird der aktuellen Stable-Reihe. Ein Wechsel auf eine LTS-Reihe ist
möglich, falls der NVIDIA-Treiber neuen Kerneln hinterherhinkt.

## Konsequenzen

- Hardware-Unterstützung und Sicherheitsupdates kommen von Upstream.
- Die Konfiguration ist Teil des Repos und wird bei jedem Build gegen das
  Fragment geprüft, damit Kconfig keine Option still verwirft.
