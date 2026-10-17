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
  - `variant.toml` – Name (`VARIANT=`) und Pakete. Beim Desktop steht dort
    `extends = "base"`, und er nennt nur, was dazukommt.
  - `system.preset`, `user.preset` – Presets der System- und Benutzer-Units (ADR 0013).
  - `running-units.txt`, `security.toml` – was läuft und wie offen es ist.

  Beim Desktop stehen in diesen Dateien nur die Ergänzungen, und eine Datei ohne Ergänzung
  fehlt. Gemeinsam bleibt `images/initrd.toml`. `bz` (`variant.rs`) liest erst die Basis und
  dann die Ergänzungen. Es scheitert, wenn eine Ergänzung ein Paket, eine Unit oder einen
  Eintrag der Basis wiederholt. Die Presets der Basis stehen vorn, und systemd nimmt die
  erste passende Zeile, also kann der Desktop keine Unit der Basis umschalten.
- **Kennung in os-release:** `VARIANT_ID=base|desktop` und `VARIANT=` (Feld der
  os-release-Spezifikation, wie bei Fedora). `ID` und `IMAGE_ID` bleiben `beize`. Die initrd
  bleibt ohne Variante, weil sie gemeinsam ist.
- **Namen auf dem Rechner bleiben `beize_<ver>`** (GPT-Labels der Slots, UKI auf dem ESP):
  Ein Rechner trägt genau eine Variante. Das installierte 0.4 braucht deshalb keine
  Umstellung, und `beize-tpm` (`ENTRY_PREFIX`) bleibt, wie er ist.
- **Ein Update-Strom je Variante:** `bz release --variant <v>` schreibt nach
  `build/out/<v>/release/<ver>/`. Die Dateien tragen die Variante im Namen
  (`beize-<v>_<ver>_<uuid>.usr.erofs`, `….usr.verity`, `beize-<v>_<ver>.efi`). Die
  signierte Liste (ADR 0020) nennt damit die Variante. `beize-updated` liest `VARIANT_ID`
  aus seinem os-release und lehnt eine Liste mit anderen Namen ab, bevor eine Datei geholt
  wird. Der Smoke-Test prüft das mit einem korrekt signierten Release der anderen Variante.
  In `sysupdate.d` sucht die Quelle nach `beize-<v>_…`, das Ziel weiter nach `beize_…`.
- **Übergang von 0.4:** Der Updater von 0.4 kannte nur die alten Namen. Das Release 0.5
  des Desktops enthielt deshalb zusätzlich `legacy/`: dieselben Dateien als harte Links
  unter den alten Namen, mit eigener signierter Liste. Seit der Rechner des Besitzers 0.5
  hat (17.10.), gibt es nur noch die Ströme je Variante.
- **Kein Wechsel der Variante durch ein Update.** Ein Wechsel bedeutet neu installieren,
  wie bei Windows Server Core seit 2016. Ein späterer Weg dafür wäre eine eigene
  Entscheidung.
- **Start ohne Oberfläche auf jedem Desktop-Rechner:** Das UKI des Desktops bekommt neben
  der Wiederherstellung ein drittes Profil, „(Basis)“ (Profil 2), mit
  `systemd.unit=multi-user.target`. Es ist dasselbe signierte UKI und dasselbe
  verifizierte `/usr`, kein eigener Kernel und kein eigenes Image, nur ein anderes
  systemd-Ziel. Units des Desktops hängen nur unter `graphical.target`, also läuft dort
  genau die Basis, mit Netz, Konten und Updates. So lässt sich klären, ob ein Fehler aus
  der Basis oder aus dem Desktop kommt, ohne neu zu installieren. Vorbild ist der
  abgesicherte Modus von Windows und macOS.
- **Vorerst mit dem Recovery-Key** (Besitzer, 17.10.): Die Daten sind an PCR 7 und 12
  eines normalen Starts gebunden (ADR 0009/0010). Ein anderes Profil verändert PCR 12
  (`@2`), also verweigert das TPM. `beize-tpm` erkennt Profil 2 wie die Recovery als
  legitim, fragt nach dem Recovery-Key und bindet danach nichts neu. Seine
  PCR-11-Richtlinie ist signiert wie die von Beize selbst. Das braucht systemd, um die
  NvPCRs des TPM einzurichten (sonst schlagen systemd-tpm2-setup-early und
  systemd-pcrproduct fehl, gemessen am 17.10.). Die Daten öffnet das nicht, denn ihr
  Schlüssel hängt zusätzlich an PCR 12. Ob es einen eigenen TPM-Schlüssel bekommt,
  wird erst entschieden, wenn PCR-Richtlinie, Wiederherstellung bei misslungenem
  Neubinden und Tests beschrieben und freigegeben sind.
- **`bz` und CI erzwingen die Richtung:** `bz build|smoke|release --variant <v>`, Vorgabe
  `desktop`, denn das ist Beize für einen Rechner. `base` gibt man ausdrücklich an. Bis es
  Desktop-Pakete gibt, ist `desktop` die Basis mit `VARIANT_ID=desktop`. Rechner, also auch
  der des Besitzers, bekommen ab der ersten Version mit Varianten den Desktop-Strom und
  müssen später nicht wechseln. Die Basis baut ihre Pakete ohne ein Paket des Desktops und
  besteht `bz smoke`. Das prüft die CI bei jedem Push (`ci/system.sh`), und dazu baut sie den
  Desktop aus denselben Paketen. Je Variante gibt es ein eigenes Image unter
  `build/out/<v>/` (dort auch `production/` und `release/`) und eine eigene SBOM (ADR 0022).
  Kernel, Module und initrd liegen gemeinsam in `build/out/`.
- **Die Einführung ändert an der Basis nichts:** Der `/usr`-Baum von `base` ist bitgleich
  zum bisherigen Image (gemessen am 17.10.), und der von `desktop` ist es ebenfalls, solange
  der Desktop nichts hinzufügt. Sie unterscheiden sich nur in `VARIANT`/`VARIANT_ID` in
  os-release.

## Konsequenzen

- „Beize Server“ ist später eine dritte Variante neben `desktop` (`extends = "base"`), kein
  Umbau.
- Jede Funktion muss einer Seite zugeordnet werden. Das gilt auch für die offenen Issues mit
  `m3-plan`: NVIDIA, Ton und Bluetooth gehören eher zum Desktop, nftables und zram/oomd zur
  Basis.
- Der Smoke-Test des Desktops startet auch das Profil ohne Oberfläche und verlangt dort
  die `running-units.txt` der Basis.
- Zwei Images bedeuten zwei Smoke-Tests und, sobald es Desktop-Pakete gibt, mehr
  CI-Minuten. Das ist der Preis der Trennung.
