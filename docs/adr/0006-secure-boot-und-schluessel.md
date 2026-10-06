# 0006 – Secure Boot mit eigenen Schlüsseln

Status: angenommen · 2026-10-06

## Kontext

Beize soll nur Code starten, den der Besitzer des Rechners freigegeben hat –
vom Bootloader über den Kernel bis zum (später verity-geschützten) System.
Die privaten Schlüssel dafür sind die wertvollsten Geheimnisse des Projekts.

## Entscheidung

**Kette.** Firmware → systemd-boot → Unified Kernel Image. Beide EFI-Binärdateien
werden mit dem db-Schlüssel signiert; die Kernel-Kommandozeile ist Teil des
signierten UKI und kann beim Booten nicht verändert werden (`editor no`).
systemd-boot und der UKI-Stub stammen aus Beizes eigenem systemd-Build mit
eigener SBAT-Kennung (`beize`).

**Schlüsselhierarchie.** Eigene PK/KEK/db (RSA-2048, von jeder UEFI-Firmware
akzeptiert), keine Microsoft-Zertifikate.

**Zwei Schlüsselsätze, streng getrennt:**

| | Testschlüssel | Produktionsschlüssel |
| --- | --- | --- |
| Zweck | Jeder Build und jede VM-/CI-Prüfung | Echte Hardware |
| Erzeugt von | `bz`, automatisch unter `build/work/secureboot-test/` | dem Besitzer, mit `bz` |
| Aufbewahrung | wertlos, liegen nur im Build-Verzeichnis | ausschließlich im 1Password-Tresor „Beize“ |
| Kennzeichnung | `CN=Beize TEST ONLY …` | `CN=Beize …` |

Produktionsschlüssel betreten nie das Build-Verzeichnis und werden nie von
Automatisierung gelesen: Signiert wird für echte Hardware nur auf ausdrückliche
Anweisung des Besitzers, und 1Password fragt dabei ihn um Freigabe.

**Prüfung.** `bz smoke` bootet bei jedem Build unter erzwungenem Secure Boot
(OVMF mit eingeschriebenen Testschlüsseln) und verlangt `secureboot=on`. Außerdem
prüft es die Gegenrichtung: dieselbe Disk mit unsigniertem Kernel-Image muss von
der Firmware abgewiesen werden.

## Konsequenzen

- Jeder Build testet die vollständige Secure-Boot-Kette, nicht nur die Signatur.
- Für die Installation auf echter Hardware folgen: Erzeugen der
  Produktionsschlüssel, Einschreiben in die Firmware (Setup-Modus, über
  systemd-boot) und `bz sign` – Schritt für Schritt dokumentiert, ausgeführt vom Besitzer.
