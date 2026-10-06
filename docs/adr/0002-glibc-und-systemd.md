# 0002 – glibc und systemd als Basis

Status: angenommen · 2026-10-06

## Kontext

Beize muss proprietäre Linux-Software ausführen können: Rider, 1Password,
Brave und den NVIDIA-Treiber. Diese Programme setzen glibc voraus. Für Boot,
Geräteverwaltung, Sitzungen und Logging braucht es ein ausgereiftes Init-System.

## Entscheidung

- C-Bibliothek: glibc. musl scheidet aus, weil die genannten Programme damit nicht laufen.
- Init, Geräte, Sitzungen: systemd (inklusive udev, logind, systemd-boot-Stub).

Beides wird aus dem Quellcode gebaut. Ein eigenes Init bleibt langfristig
möglich, ist aber kein Ziel.

## Konsequenzen

- Binärkompatibilität zum Linux-Desktop-Standard.
- Das frühe `beize-init` ist nur ein Werkzeug für M1-Boot-Tests und wird ersetzt.
