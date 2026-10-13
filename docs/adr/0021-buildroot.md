# 0021 – Buildroot: abgeschotteter, gepinnter Bau

Status: angenommen · 2026-10-13 (mit dem Besitzer abgestimmt)

## Kontext

`bz` lief direkt auf der Werkbank (ADR 0004), die CI in einem Ubuntu-Container, der seine
Pakete bei jedem Lauf frisch per `apt-get` holte. Damit sah ein Bau alles, was auf dem
Rechner zufällig installiert ist, und die Werkzeuge änderten sich mit jedem Ubuntu-Update.
Am 2026-10-13 war die CI deshalb rot, lokal aber grün: polkit fand `glib-mkenums` nur, weil
es auf der Werkbank installiert war. „Lokal grün“ hieß also nicht „CI grün“, und welche
Werkzeuge einen Stand gebaut hatten, war nirgends festgehalten.

Große Distributionen bauen in einer Umgebung, die nur aus erklärten Paketen besteht:
Fedora in mock/Koji-Buildroots, Debian mit sbuild (die `.buildinfo` hält die Umgebung fest),
Arch in sauberen Chroots, Yocto mit einer Liste erlaubter Host-Werkzeuge (HOSTTOOLS).

## Entscheidung

- **Ein Buildroot für alles.** Ein Ubuntu-26.04-Wurzeldateisystem mit genau den Paketen
  aus `ci/buildroot/packages.txt`, der Rust-Version aus `rust-toolchain.toml` und
  cargo-deny für die CI. `bz` läuft nur darin; `bz install` als einzige Ausnahme schreibt
  echte Disks mit `sudo` und baut nichts.
- **Gepinnt wie `Cargo.lock`.** `ci/buildroot/lock.sh` löst die Paketliste gegen Ubuntus
  Snapshot zum Datum in `ci/buildroot/snapshot` auf. Es schreibt `packages.lock`: jedes Paket
  mit Version, Datei im Pool und SHA-256. Die Prüfsummen hängen über die Paketindizes an
  der signierten InRelease-Datei.
  `create.sh` lädt genau diese Dateien vom Ubuntu-Archiv (schnell; der Snapshot nur für
  Versionen, die das Archiv nicht mehr hat), prüft jede Prüfsumme und vergleicht am Ende die
  installierte Paketliste mit dem Lock. Neue Ubuntu-Updates kommen nur über einen neuen
  Snapshot plus `lock.sh`, als Commit.
- **Name und Nachweis.** Der Buildroot heißt nach dem Hash seiner Definition
  (`ci/buildroot/hash.sh`). `/etc/beize-buildroot` trägt diesen Namen, `bz` prüft ihn bei
  jedem Start. `/usr/share/beize-buildroot/buildinfo` hält alle Pakete mit Version fest,
  wie Debians `.buildinfo`.
- **Lokal.** `./bz` legt den Buildroot bei Bedarf unter `build/buildroot/<name>` an
  (mmdebstrap, unprivilegiert) und startet `bz` darin mit bubblewrap:
  - die Wurzel read-only, das Repository unter seinem Pfad;
  - `/dev/kvm` für die Test-VMs;
  - der Bus des Benutzer-Service-Managers, damit jedes Paket in seiner cgroup mit
    Speichergrenze baut (ADR 0005); `systemd-run --scope` bleibt dabei im Sandkasten.
  - Nur `secureboot`, `sign` und `release` sehen zusätzlich die 1Password-CLI.
- **CI.** Ein Job legt denselben Buildroot als Container-Image in der Registry des
  Repositorys an (`beize-buildroot:<name>`), nur wenn es ihn noch nicht gibt. Alle anderen
  Jobs laufen darin. CI und Werkbank bauen damit in derselben Umgebung.
- **Neu bauen, was es betrifft.** Der Fingerprint jedes Pakets und jedes Schritts enthält
  den Hash von `packages.lock` und `rust-toolchain.toml`. Neue Werkzeuge bauen also alles
  neu. Was die Bauten nicht benutzen (cargo-deny), baut nichts neu.

## Konsequenzen

- Eine Abhängigkeit vom Host, die nicht in `packages.txt` steht, scheitert lokal genauso
  wie in der CI. Die Nachstellung der CI im Container entfällt.
- Der Buildroot ist ein Werkzeug, kein Teil von Beize: in Beizes `/usr` kommt nichts
  daraus (die ELF-Prüfung verlangt Beizes eigene Compiler).
- Ein neuer Buildroot kostet lokal etwa 4 Minuten und einen Bau von null. Dafür ist jeder
  Stand mit genau bekannten Werkzeugen gebaut, der Schritt zu bit-genauen Builds (#5).
- Die Werkbank braucht nur noch Linux mit mmdebstrap, bubblewrap und curl. `bz` ist
  damit nicht mehr an Ubuntu oder WSL gebunden (ADR 0004).
