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
Alles für Beize wird damit gegen den eigenen Sysroot gebaut, der Host – Beizes
Buildroot (ADR 0021) – liefert nur Werkzeuge.

**Zwei Maschinen, zwei pkg-config.** Wie bei Debian, Yocto und Buildroot fragt ein
Bau das Zielsystem über `x86_64-beize-linux-gnu-pkg-config`: pkgconf mit einer
Personality (Sysroot und Suchpfade des Pakets). Das Sysroot steht nur vor `-I`/`-L`
(freedesktop-Regel); Variablen bleiben Pfade auf dem Ziel. Ein schlichtes `pkg-config`
im PATH eines Zielpakets ist dasselbe, damit nichts aus Versehen den Host fragt.
Werkzeuge, die beim Bauen laufen (Meson `native: true`, z. B. `glib-mkenums`), kommen
ausdrücklich vom Host (Meson-Datei für die Baumaschine).

**Sysroot je Paket.** Jedes Paket baut gegen ein eigenes Sysroot, das genau seine
(transitiven) Abhängigkeiten enthält – harte Links ins gemeinsame Sysroot, wie
Yoctos recipe-sysroot. So hängt kein Ergebnis davon ab, was zufällig schon gebaut
ist. Der Compiler bekommt es über `--sysroot` in `$CC`; ein eigenes Standard-Sysroot
hat er nicht, damit ein Aufruf am `$CC` vorbei scheitert, statt fremde Pakete oder
den Host zu sehen.

**Rezepte.** Jedes Paket ist eine TOML-Datei unter `recipes/`: gepinnte Quelle,
Abhängigkeiten, Stufe (`toolchain` oder `target`) und ein Build-Skript.

**Signierte Quellen.** Jede Quelle ist doppelt abgesichert: gepinnt (SHA-256 des
Archivs bzw. Commit eines Git-Tags) und von ihren Upstream-Maintainern signiert.
`bz` prüft die Signatur gegen einen minimalen Schlüsselbund unter `keys/` (OpenPGP
mit `gpgv`, Gits SSH-Signaturen mit `ssh-keygen -Y verify`). Wo ein Projekt keine
signierten Archive veröffentlicht (systemd), wird der signierte Release-Tag
verwendet. Veröffentlicht Upstream gar keine Signatur, ist die Quelle nur gepinnt –
als begründete Ausnahme im Rezept (`unsigned = "<Grund>"`), die `bz verify` jedes
Mal auflistet (`keys/README.md`).

**Inkrementell.** Ein Paket wird neu gebaut, wenn sich sein Rezept, seine
Quellen, die Build-Umgebung oder eine Abhängigkeit geändert haben (Fingerprint).

**Parallel.** Unabhängige Pakete bauen nebeneinander, der Kernel neben ihnen. Die
CPUs verteilt ein gemeinsamer GNU-make-Jobserver, so groß, wie CPUs und verfügbarer
Speicher erlauben (512 MiB je Job, 2 GiB Reserve). Ein Rezept gibt ihn nur seinen
Bauschritten (`MAKEFLAGS="$JOBSERVER"` vor make, meson compile, cmake --build,
cargo); alles andere, auch das Installieren, läuft seriell wie Fedoras
`%make_install` – parallel installiert, linkte lvm2 ein Programm zweimal zugleich. Ein Paket startet nur, wenn sein beim letzten Bau gemessener
Speicherbedarf neben dem der laufenden passt und der Kernel nicht schon unter
Speicherdruck steht (PSI) – wie Yoctos `BB_PRESSURE_MAX_MEMORY`. Jedes läuft in
einer eigenen cgroup mit Speichergrenze (systemd-Scope; in der CI ohne): reicht der
Speicher doch nicht, scheitert der eine Bau statt der Maschine. Vorrang hat das
Paket mit dem längsten verbleibenden Pfad.

**Besitz.** Zielpakete installieren in ein Staging-Verzeichnis. `bz` normalisiert
die Pfade auf das zusammengeführte `/usr`, prüft auf Konflikte mit anderen
Paketen und schreibt ein Manifest. `bz owner <pfad>` beantwortet, woher eine Datei kommt.

**Was ins Image kommt.** Wie Fedoras `%files` erklärt jedes Rezept seine Dateien.
Nur zum Bauen sind ihrer Art nach Header, statische Bibliotheken, pkg-config- und
CMake-Dateien, Handbücher und Linker-Namen (Fedoras -devel). Alles andere nennt der
Abschnitt `[files]` des Rezepts:
- `image`: Muster der Dateien fürs Image. Programme und Bibliotheken direkt in
  `usr/bin` und `usr/lib` stehen einzeln da, tiefere Verzeichnisse auch als `dir/**`.
- `build`: Dateien nur zum Bauen anderer Pakete, jede mit Grund. Ein solcher
  Eintrag nimmt Dateien auch aus einem Image-Muster heraus, wie `%exclude`.

Eine Datei ohne Zuordnung lässt den Bau scheitern, ebenso ein Muster, das nichts
trifft (Fedoras „Installed (but unpackaged) file(s) found“). So kommt nichts
unbemerkt ins Image. `[files]` ist die letzte Tabelle des Rezepts und zählt nicht
zum Bau-Fingerprint.

**Image.** Das `/usr`-Image wird aus diesen Dateien der Pakete in `images/base.toml`
zusammengesetzt, ohne Debug-Informationen. Es ist ein read-only EROFS-Image (zstd).

## Konsequenzen

- Kein Paketmanager im laufenden System; Pakete existieren nur zur Build-Zeit.
- Der erste vollständige Build dauert länger (zwei GCC-Durchläufe), danach wird nur gebaut, was sich geändert hat.
