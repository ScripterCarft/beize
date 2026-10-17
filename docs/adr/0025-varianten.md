# 0025 – Varianten: Basis und Desktop

Status: angenommen · 2026-10-17 (Besitzer, im Chat vom 17.10.; Vorgabe `desktop` und Start
ohne Oberfläche nach seinen Zielen: Rechner bekommen den Desktop, die Basis dient der
Fehlersuche und Servern wie einem NAS)

## Kontext

M3 bringt die Oberfläche (Grafik, Compositor, Shell, Schriften, Ton). Der Besitzer hat am
16.10. entschieden, dass Beize dabei modular bleibt: Alles, was M2 ist, bildet eine
vollständige **Basis** ohne Oberfläche, der **Desktop** liegt darüber. Später soll daraus
ohne Umbau ein „Beize Server“ werden können.

Heute kennt `bz` genau ein Image: `images/base.toml`, ein Satz Presets
(`base.preset`, `user.preset`), eine `running-units.txt`, eine `security.toml`, ein
Release-Strom (`beize_<ver>…`). Kommt das erste Desktop-Paket in diese Dateien, ist die
Trennung verloren, bevor sie existiert.

Vorbilder: Windows Server Core – dasselbe System, dieselben Updates, ohne Shell; Fedora
Workstation/Server – Editionen aus denselben Paketen, unterschieden durch `VARIANT_ID` in
os-release. Beides sind Varianten eines Baus, keine Forks.

## Entscheidung

- **Zwei Varianten eines Baus: `base` und `desktop`.** Gleicher Commit, gleiche Rezepte,
  gleiche Versionsnummer. `desktop` ist `base` plus Ergänzungen – nie weniger, nie anders.
- **Abhängigkeiten nur Desktop → Basis.** Kein Paket der Basis hängt von einem Paket des
  Desktops ab. Der Desktop spricht mit der Basis nur über deren Schnittstellen
  (Varlink/D-Bus, wie heute `beize-accountsd` und `beize-updated`); braucht er etwas Neues
  von der Basis, bekommt die Basis eine neue Schnittstelle, nicht der Desktop Zugriff auf
  ihren Zustand.
- **Ein Kernel, eine initrd, ein Sicherheitsmodell.** Kernel, Module, Firmware und initrd
  sind in beiden Varianten bitgleich (alle Module liegen schon heute im Image, ADR 0015).
  Dieselben Schlüssel (ADR 0006, 0015, 0020), dieselbe TPM-Bindung (ADR 0009/0010),
  dieselbe Wiederherstellung.
- **Definition je Variante in `images/<variante>/`:**
  - `packages.toml` – die Pakete; die von `desktop` heißt `extends = "base"` und nennt
    nur, was dazukommt.
  - `system.preset` – Presets; die des Desktops kommen vor denen der Basis, dürfen aber
    keine Unit der Basis umschalten.
  - `running-units.txt`, `security.toml` – was läuft und wie offen es ist; beim Desktop
    jeweils nur die zusätzlichen Einträge.

  Gemeinsam bleiben `images/initrd.toml` und `images/user.preset`.
  `bz` liest bei `desktop` erst die Basis und dann die Ergänzungen. Es scheitert, wenn eine
  Ergänzung etwas aus der Basis wiederholt oder ändert.
- **Kennung in os-release:** `VARIANT_ID=base|desktop` und `VARIANT=` (Feld der
  os-release-Spezifikation, wie bei Fedora). `ID` und `IMAGE_ID` bleiben `beize`. Die initrd
  bleibt ohne Variante, weil sie gemeinsam ist.
- **Namen auf dem Rechner bleiben `beize_<ver>`** (GPT-Labels der Slots, UKI auf dem ESP):
  Ein Rechner trägt genau eine Variante. Das installierte 0.4 braucht deshalb keine
  Umstellung, und `beize-tpm` (`ENTRY_PREFIX`) bleibt, wie er ist.
- **Ein Update-Strom je Variante:** `bz release --variant <v>` schreibt nach
  `release/<v>/<ver>/`. Die Dateien tragen die Variante im Namen
  (`beize-<v>_<ver>_<uuid>.usr.erofs`, `….usr.verity`, `beize-<v>_<ver>.efi`). Die
  signierte Liste (ADR 0020) nennt damit die Variante. `beize-updated` lehnt sie streng ab,
  wenn die Variante nicht zur laufenden passt, bevor eine Datei geholt wird. In
  `sysupdate.d` sucht die Quelle nach `beize-<v>_…`, das Ziel weiter nach `beize_…`.
- **Kein Wechsel der Variante durch ein Update.** Ein Wechsel bedeutet neu installieren,
  wie bei Windows Server Core seit 2016. Ein späterer Weg dafür wäre eine eigene
  Entscheidung.
- **Start ohne Oberfläche auf jedem Desktop-Rechner:** Das UKI des Desktops bekommt neben
  der Wiederherstellung ein drittes Profil, „(ohne Oberfläche)“, mit
  `systemd.unit=multi-user.target`. Units des Desktops hängen nur unter
  `graphical.target`, also läuft dort genau die Basis, mit Netz, Konten und Updates. So
  lässt sich klären, ob ein Fehler aus der Basis oder aus dem Desktop kommt, ohne neu zu
  installieren. Vorbild ist der abgesicherte Modus von Windows und macOS. Die TPM-Bindung
  bleibt wie beim normalen Start, denn es läuft dasselbe signierte System.
- **`bz` und CI erzwingen die Richtung:** `bz build|smoke|release --variant <v>`, Vorgabe
  `desktop`, denn das ist Beize für einen Rechner. `base` gibt man ausdrücklich an. Bis es
  Desktop-Pakete gibt, ist `desktop` die Basis mit `VARIANT_ID=desktop`. Rechner, also auch
  der des Besitzers, bekommen ab der ersten Version mit Varianten den Desktop-Strom und
  müssen später nicht wechseln. Die Basis baut ihre Pakete ohne ein Paket des Desktops und
  besteht `bz smoke`.
  Das prüft die CI bei jedem Push wie heute. Ein zweiter Job für `desktop` kommt mit dem
  ersten Desktop-Paket. Er nutzt den Cache der Basis, kostet also nur die Desktop-Pakete,
  das Image und den Smoke-Test. Je Variante gibt es ein eigenes Image unter
  `build/out/<v>/` und eine eigene SBOM (ADR 0022).

## Umsetzung in `bz`

`tree::IMAGE_DEFINITION` und `usr.rs` (`UNIT_PRESETS`, `RUNNING_UNITS`) lesen die Variante,
`security.rs` ebenso. `layout.rs` bekommt `os_release(version, variant)` und
Release-Muster je Variante. Die Namen auf dem Rechner (`usr_label`, `uki_name`) bleiben.
`vm/checks.rs` nimmt `running-units.txt` der Variante zur Laufzeit statt per
`include_str!`. Dazu kommen `release.rs`, `sbom.rs` und in `os/beize-update` `list.rs`
(Muster mit Variante). Die Einführung ändert für die Basis nichts: Image und Release von
`base` sind bis auf `VARIANT_ID`/`VARIANT` und die Release-Namen gleich wie bisher.

## Konsequenzen

- „Beize Server“ ist später eine dritte Variante neben `desktop` (`extends = "base"`), kein
  Umbau.
- Jede Funktion muss einer Seite zugeordnet werden. Das gilt auch für die offenen Issues mit
  `m3-plan`: NVIDIA, Ton und Bluetooth gehören eher zum Desktop, nftables und zram/oomd zur
  Basis.
- Der Übergang von 0.4 auf die erste Version mit Varianten braucht ein Update aus dem
  bisherigen Strom (`beize_<ver>…`), einmal für `desktop`. Ab dann gibt es nur noch die
  Ströme je Variante.
- Der Smoke-Test des Desktops startet auch das Profil ohne Oberfläche und verlangt dort
  die `running-units.txt` der Basis.
- Zwei Images bedeuten zwei Smoke-Tests und, sobald es Desktop-Pakete gibt, mehr
  CI-Minuten. Das ist der Preis der Trennung.
