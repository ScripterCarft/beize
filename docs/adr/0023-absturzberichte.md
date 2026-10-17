# 0023 – Absturzberichte: MiniDebugInfo und elfutils

Status: angenommen · 2026-10-14 (Besitzer: „mach komplett ordentlich“)

## Kontext

Stürzte ein Programm ab, hielt systemd-coredump den Speicher fest, aber `coredumpctl`
zeigte keinen Stacktrace: systemd war ohne elfutils gebaut, und ohne libdw entrollt es
keinen Stack. Die Namen dafür waren da – `bz` entfernte nur die Debug-Infos, die volle
Symboltabelle blieb in jedem Programm (12 MiB der 97 MiB ELF-Dateien in `/usr`).

Fedora löst beides beim Bau jedes Pakets (`find-debuginfo` in rpmbuild): Debug-Infos
getrennt nach Build-ID in `-debuginfo`-Pakete, das Programm ganz gestrippt, und als Ersatz
für die Symboltabelle **MiniDebugInfo** – nur die Funktionssymbole, die die dynamische
Symboltabelle nicht schon hat, als xz-komprimierte ELF-Datei im Abschnitt `.gnu_debugdata`.
gdb und libdw lesen sie; systemd-coredump schreibt damit Stacktraces mit Funktionsnamen
ins Journal, ohne dass etwas nachgeladen wird.

## Entscheidung

- **MiniDebugInfo für jedes Programm und jede Bibliothek, beim Bau des Pakets** – wie
  Fedora, dieselben Schritte (`debuginfo.rs`): nach dem Installieren eines Pakets bekommt
  jede ELF-Datei (`ET_DYN`; Beize linkt alles als PIE) MiniDebugInfo und wird ganz
  gestrippt. Einmal je Paket, parallel mit den anderen Bauten, zwischengespeichert wie das
  Paket; `/usr` und initrd kopieren nur noch. Anders als Fedora bleibt `.comment` im
  Programm: daran erkennt die ELF-Prüfung Beizes Compiler (ADR 0014). Hat ein Paket selbst
  gestrippt, scheitert der Bau – Namen kann dann nichts mehr retten.
- **Die ELF-Prüfung verlangt es** (ADR 0014): jede Datei in `/usr` und der initrd trägt
  MiniDebugInfo und keine volle Symboltabelle.
- **systemd mit elfutils** (`-Delfutils=enabled`; libdw und libelf, dlopen). elfutils
  ohne debuginfod (Beize fragt keinen Server nach seinen Programmen), ohne die `eu-*`-
  Werkzeuge. Mit C++-Demangler, seit Mesa als erstes C++ libstdc++ ins Image bringt
  (2026-10-17); vorher ohne, solange er libstdc++ allein dafür gebracht hätte.
- **Keine vollen Debug-Infos (`-g`), vorerst.** Gemessen (2026-10-14, alle Pakete): keine
  messbare Bauzeit, aber 244 MiB mehr im Sysroot. Sie brächten Zeilennummern und
  Variablen – nur mit gdb oder debuginfod, und beides hat Beize nicht. Kommt eines davon,
  legt derselbe Schritt die Debug-Infos nach Build-ID außerhalb des Images ab.
- **Prüfung im Smoke-Test:** Ein Programm des Besitzers (`sleep`) stürzt ab; `coredumpctl
  info` zeigt ihm den Stack mit `main` aus sleep und `__libc_start_call_main` aus glibc –
  Namen, die nur in MiniDebugInfo stehen.

## Konsequenzen

- Jeder Absturz steht mit Stacktrace im Journal, auch auf dem Rechner des Besitzers.
  Rust-Funktionen erscheinen mit ihrem Symbolnamen (`_ZN…`), C++-Namen lesbar.
- Kleiner trotz elfutils (2026-10-14): `/usr` 40,8 → 40,3 MiB (unkomprimiert 128 → 110 MiB),
  initrd 10,3 → 10,1 MiB; Bau von null 9,6 → 9,2 min (der Schritt läuft je Paket parallel).
- Ein Stacktrace nennt Funktionen, keine Zeilen; dafür braucht es später `-g` (oben).
