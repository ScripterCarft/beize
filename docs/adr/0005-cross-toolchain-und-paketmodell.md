# 0005 – Eigene Cross-Toolchain und Paketmodell

Status: angenommen · 2026-10-06

## Kontext

Beize wird auf einer fremden Linux-Installation gebaut (ADR 0004). Würde man
mit dem Host-Compiler direkt bauen, landeten unbemerkt Header und Bibliotheken
des Hosts im System. Außerdem soll jede Datei im System einem Besitzer
zugeordnet sein (Grundprinzip „Nachvollziehbar“).

## Entscheidung

**Toolchain.** `bz` baut eine Cross-Toolchain für `x86_64-beize-linux-gnu`:
binutils → gcc (Stufe 1, ohne libc) → Kernel-Header → glibc → gcc (vollständig).
Alles für Beize wird damit gegen den eigenen Sysroot gebaut, der Host liefert
nur Werkzeuge.

**Rezepte.** Jedes Paket ist eine TOML-Datei unter `recipes/`: gepinnte Quelle,
Abhängigkeiten, Stufe (`toolchain` oder `target`) und ein Build-Skript.

**Signierte Quellen.** Jede Quelle ist doppelt abgesichert: gepinnt (SHA-256 des
Archivs bzw. Commit eines Git-Tags) und von ihren Upstream-Maintainern signiert.
`bz` prüft die Signatur mit `gpgv` gegen einen minimalen Schlüsselbund unter
`keys/` und nimmt unsignierte Quellen nicht an. Wo ein Projekt keine signierten
Archive veröffentlicht (systemd), wird der signierte Release-Tag verwendet.

**Inkrementell.** Ein Paket wird neu gebaut, wenn sich sein Rezept, seine
Quellen, die Build-Umgebung oder eine Abhängigkeit geändert haben (Fingerprint).

**Besitz.** Zielpakete installieren in ein Staging-Verzeichnis. `bz` normalisiert
die Pfade auf das zusammengeführte `/usr`, prüft auf Konflikte mit anderen
Paketen und schreibt ein Manifest. `bz owner <pfad>` beantwortet, woher eine Datei kommt.

**Image.** Das Root-Dateisystem wird nur aus den Manifesten der Pakete in
`images/base.toml` zusammengesetzt, ohne Header, statische Bibliotheken und
Dokumentation, und ohne Debug-Informationen. Es ist ein read-only EROFS-Image (LZ4).

## Konsequenzen

- Kein Paketmanager im laufenden System; Pakete existieren nur zur Build-Zeit.
- Der erste vollständige Build dauert länger (zwei GCC-Durchläufe), danach wird nur gebaut, was sich geändert hat.
- Das frühe `beize-init` aus M1 entfällt, PID 1 ist systemd.
