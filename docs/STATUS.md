# Stand und offene Punkte

Was gerade gilt. Verlauf steht in den Commits (`git log`), Entscheidungen in den ADRs,
Messwerte der Hardware-Tests in `~/hw-logs/results-*`. Hier wird ersetzt, nicht angehängt.

## Stand (28.11.2026)

- **M3 Compositor:** Desktop mit Shell (ADR 0028 (a)–(f)) läuft auf dem PC des Besitzers
  (RTX 3070 Ti, NVK, 2560×1440 @ 240 Hz). `main` ist 0.6, der letzte Messbau 0.5.132
  (`~/hw-logs/results-20261128-110133`, mit binärem Journal).
- **Animationen (ADR 0032) fertig, auf Hardware abgenommen** (0.5.131: „viel besser“, 0.5.132:
  „sah schon gut aus“). Schließen animiert seit 0.5.132 auf Hardware (Beize-UI-Apps rissen beim
  Beenden die Verbindung ab, a716d2d). Alt+Tab-Ring 0,1 s (Besitzer, 28.11.).
- **Anmeldedialog (ADR 0031), Fokus (ADR 0030):** auf Hardware gut. run0 ohne 25-s-Grenze über
  den eigenen systemd-Patch 0002.
- **Updates 27.11.:** Linux 7.3.6 (Start ab Loader 8,5 s statt 13,6 s, #11 erledigt), Mesa
  26.3.1, LLVM 23.1.6, Vulkan 1.4.369, linux-firmware 20261124, neun Basis-Pakete, Rust-Locks.
  xkeyboard-config bleibt 2.48 (#20). Dependabot #45 (parley 0.13, skrifa 0.49, resvg 0.50,
  xkbcommon 0.9) rot: parleys API (`Layout::align`) – gehört zur Schrift im UI-Overhaul.
- **Späte Bilder seit 0.5.132: Mechanismus im Code belegt** (Gutachter 28.11.). Die Phase
  `begin` von `render/mod.rs` `submit()` enthält nur eine Arbeit, die Millisekunden kosten kann:
  `shared.collect()` (`beize-vulkan/src/device.rs`) zerstört vor dem Aufzeichnen alles, was die
  GPU nicht mehr braucht, je Bild ein `vkFreeMemory`. NVK (Mesa 26.3.1,
  `nvkmd_nouveau_va_free`) hebt dabei die Abbildung mit `DRM_NOUVEAU_VM_BIND` synchron auf, also
  ein Kernel-Aufruf mit Warten je Bild. Jedes `Picture` und jede Blur-Kette hat eigenen Speicher
  (`image.rs:170`, `target.rs:209`), entsteht je Animation neu und wird beim Ende freigegeben,
  passend zu „dialog closes“ 57, „menu closes“ 52, „comes back“ 44. Beim Live-Resize geben alte
  dmabuf-Importe frei. „flip event repeated“ verfälscht „late“ nicht (`pacing.rs:213`).
  **Seit 28.11. auf `main`, auf Hardware noch nicht gesehen:** Freigaben laufen erst nach dem
  Commit an KMS (`free_finished`), Beizes Bilder und Puffer liegen in Blöcken des
  Sub-Allokators `gpu-allocator` (`beize-vulkan/src/memory.rs`; dmabuf-Importe bleiben eigen,
  Upload-Puffer bevorzugen jetzt CPU-sichtbaren VRAM wie bei wgpu). Dauert eine Freigabe ≥ 1 ms,
  steht „freed N GPU objects in X ms“ im Journal – der nächste Hardware-Test belegt damit die
  Millisekunden ohne eigenen Messbau.
- **Upstream:** `docs/upstream.md` (27.11. geprüft): nouveau GSP-IRQDEST in Mainline (7.4, Cc
  stable); systemd #44842 „needs-rework“, v2 und die run0-Fassung für main liegen in `~/upstream`.
- **1Password läuft auf Beize noch nicht:** kein Weg, Apps zu installieren.
- **CI:** schneller Lauf je Push, nachts voll; zuletzt alles grün. Push braucht entsperrtes 1Password.

## Als Nächstes: der Gesamtplan (28.11.)

Grundlage: Bestandsaufnahme vom 28.11. mit 28 Bildschirmfotos aller Zustände (hell und dunkel)
sowie den Gutachten von `beize-design-auditor` (Werte, Material, Abläufe) und `beize-reviewer`
(Render-Pfad, Struktur, Tests). Die Reihenfolge folgt einer Regel: **erst das, worauf alles
Spätere steht, dann die Formensprache, dann die Systeme, die sie brauchen.** Nach jeder Stufe
läuft Beize (`bz gate` grün). Erledigte Stufen werden hier gelöscht.

### Phase A: Fundament (unsichtbar, macht alles Spätere billiger)

- **A1 GPU-Speicher:** Freigeben nach dem Commit und der Sub-Allokator sind gebaut (oben). Offen
  nur, wenn die Hardware danach noch späte Bilder beim Öffnen zeigt: ein Pool für Bildziele und
  Blur-Ketten nach Größe (niri `OffscreenBuffer`, KWin `OffscreenEffect`). Ohne Beleg nicht bauen.
- **A2 Struktur:** erledigt (28.11.). Compositor `render/`, `animation/`, `state/`, `windows/`;
  Beize UI `app/`; die Shell `struct Shell` mit `turn()` (eine Methode je Quelle), `bar.rs`,
  `menus.rs`; die Aufräumliste. Der allgemeine Weg „eine Fläche geht mit ihrem Bild“ kommt mit
  den Mitteilungen (C1), wenn es die sechste Art gibt – vorher wäre es eine Abstraktion ohne
  Anlass.
- **A3 Tests schneller und als Galerie.**
  - Ein Journal-Leser auf dem Host, der einmal folgt, statt ~260 Abfragen, die je 200 ms das
    ganze Journal neu lesen.
  - Positive Marken statt ~45 fester Pausen, allein 29 in `menus.rs`: „Menü offen“, „Taste an
    App“.
  - Erledigt 28.11.: der Desktop in zwei Szenarien (`desktop`, `desktop-menus`), `bz gate`
    324 → 229 s. Ein dritter Teil lohnt erst mit dem Journal-Leser (jeder Teil zahlt ~50 s Start).
  - Neu: ein Teil **Galerie**, der jeden Zustand hell und dunkel als PNG ablegt. Er ersetzt die
    Handaufnahmen und ist der Beleg des Gutachters für jede Design-Stufe (VISION:
    Screenshot-Vergleiche).
  - Bauen: `cargo --timings` messen, dann gemeinsames Target je gleiche Features, lld.
- **A4 Ein Token-Modul** `beize-draw::tokens`: Radien, Abstände, Schrift, Farben, Bewegung an
  einer Stelle, erst mit den heutigen Werten (pixelgleich, Galerie vergleicht). Heute sind sie
  verstreut, zum Beispiel drei Schwarz-Tinten und die Akzentfarbe doppelt. Materialien bleiben
  beim Compositor und werden per Name bestellt (wie `NSVisualEffectView.Material`). Danach
  ändert der Umbau nur Werte.

### Phase B: Formensprache

- **B1 ADR 0033 Formensprache** (Claude schreibt es und zeigt es dem Besitzer):
  - ein 4-px-Raster;
  - konzentrische Radien in einer Skala;
  - zwei Schriftgrößen und drei Gewichte (heute fünf);
  - semantische Farben mit WCAG-AA-Prüfung;
  - **ein Material**: Beize-Glas aus dem Menü-Glas in drei Dichten (dicht ~80 % für Menüs,
    Dialoge, Alt+Tab, Kontrollzentrum und Mitteilungen; mittel für das Dock; dünn für das
    Grundglas);
  - zwei Schattenstufen;
  - Bewegung bleibt ADR 0032.

  Flüssiges Glas, Rauch und Tonkurve entfallen; das System wird einfacher und billiger.
- **B2 Mock-ups mit dem echten Renderer**: zwei bis drei Token-Sätze über die Galerie. Claude
  wählt und zeigt sie.
- **B3 Umbau in Stufen**, jede vom Gutachter geprüft. Die Hardware-Abnahme läuft gebündelt mit
  der Messung der späten Bilder.
  1. Material: Dock und Kontrollzentrum auf Beize-Glas, eine Glasfläche mit Kacheln, dunkle
     Schrift im hellen Modus. Das behebt 3,3:1 und 2,4:1 sowie das unsichtbare Dock.
  2. Menüs, Dialog, Feld, Alt+Tab und Pillen auf Tokens:
     - konzentrisch;
     - Fehlerrot mit 4,5:1;
     - Alt+Tab-Auswahl leiser als Vollblau.
  3. Fenster, Titelleiste und Menüleiste.
  4. Schrift: parley 0.13 (Dependabot #45) zusammen mit der Schriftskala.
- **B4 Abläufe:**
  - „Ausblenden“ als eigener Zustand (heute minimiert es; „Alle einblenden“ holt auch absichtlich
    Minimiertes);
  - Dock-Klick holt das zuletzt benutzte Fenster, auch minimiert (GNOME-Regel);
  - keine Raute, stattdessen „Minimiert“ in der Kürzelspalte;
  - Terminal-Titel mit Ordner;
  - polkit-Text in menschlicher Sprache statt `run-p….service` (VISION 4).

### Phase C: Systeme, je mit eigenem ADR, auf A und B

1. **Mitteilungen:** Server `org.freedesktop.Notifications` in der Shell (wie GNOME Shell und
   plasmashell), auf Beize-Glas, mit dem allgemeinen Bildweg aus A2.
2. **Ton:** erst PipeWire (#17, es gibt noch kein Rezept), dann Systemklänge in der Shell.
3. **Apps installieren:**
   - `wp_security_context_v1` (Flatpak, Mutter, KWin, niri) vor der cgroup-Erkennung in einem
     Modul „Client-Identität“;
   - xdg-desktop-portal mit Beize-Backend;
   - Bundle-Format: Zeitpunkt und Wahl Flatpak oder eigenes beim Besitzer.
4. **Updates übers Internet** (ADR 0020, #15): zuerst das Zurück-ADR; die Frage erscheint als
   Mitteilung.

**Nebenher, in Wartezeiten auf Hardware:** systemd-Entwürfe (`docs/upstream.md`) kompilieren,
logind v2 mit TEST-35, danach trägt Beize die gesendete Fassung.

**Der Besitzer parallel:**
1. Upstream senden (`docs/upstream.md`).
2. Bilder sammeln, die ihm gefallen (smart, Li Auto, Apps, Fotos), in `D:\beize-ref\`. Sie
   fließen ins ADR.
3. Hardware-Abnahmen je B3-Stufe, gebündelt.
4. Später den Zeitpunkt für Apps installieren wählen.

## Offen, mit Ort

- **logind:** `session_stop()` wählt das Display neu und speichert nur die gehende Sitzung, nicht
  die neue (`logind-session.c` ~1033, systemd main) – derselbe Fall wie Punkt 1 des Bot-Reviews
  von #44842; prüfen, ob es nach einem logind-Neustart falsch wird, dann upstream.
- **Dienste:** A7 TPM aus + SMBIOS-Kommandozeile → Recovery-Abfrage in verändertem Start
  (`beize-tpm/src/main.rs:83`; signierte Kommandozeile vergleichen); Keyring-Fehler zählt als
  „TPM entsperrt“ (`main.rs:149`); Recovery-Keys in beize-tpm als `String`; ein abgestürzter
  Compositor führt zurück in die angemeldete Konsole (vor dem Sperrbildschirm lösen).
- **Compositor:** Glas der Vorschau meldet sich als undurchsichtig, `take_shown` löst FIFO der
  Fenster dahinter (`backdrop.rs` `opaque_regions`). Beim Einrasten wird der Schatten mitgestreckt (`animation.rs` `stretched`);
  Popups eines Fensters liegen während des Trichters ungebogen am Ziel (`scene.rs` ~115).
- **Terminal:** Schrift-Verzeichnis je Zelle neu gelesen (`beize-draw/src/text.rs:183`).
- **NVK:** Tabelle im Shader las Unsinn (Index belegt im Bereich) – Treiberfehler erschlossen,
  Reproduzierer auf Hardware fehlt; Kommentare in `quad.frag`, `backdrop.frag`, `quad.vert`.
- **Tests:** Der Dialog-Test wartet 26 s (prüft polkits Timeout echt); alles Übrige in Plan A3.
- **VM / Smoke, je einmal:** #41 vCPU steht bis 21 s; 17.11. erster Start wartete auf
  `/dev/gpt-auto-root-luks` (`build/work/install-hang-02-smoke.log`); 16.11. Snap ohne Vorschau
  (VM überlastet); 18.11. Terminal zeigte nur „/r“ – der Smoke zählt jetzt die IRQ-1-Bytes
  (grün: 64 von 64), ein roter Lauf sagt, ob die Tasten den Gast erreichten. Bei Rot zuerst
  `build/work/logs/boot` ganz sichern.
- **Anmeldedialog:** Auswahl zwischen mehreren Admins per Menü fehlt (ADR 0031); der Cursor im
  Feld blinkt nicht. Im Smoke nicht abgedeckt: `CancelAuthentication` und die Warteschlange; das
  Klicken ins Terminal wartet fest 300 ms statt auf den Fokus.
- **Apps installieren:** Beize hat keinen Weg dafür; Voraussetzung für 1Password und jede
  Drittanbieter-App. Eigene Entscheidung mit ADR, Zeitpunkt beim Besitzer.
- **Issues:** `gh issue list` (M3-Plan: #14, #17, #34, #35).

## Beim Besitzer

- **Upstream senden:** die Liste mit Stand je Patch und Bericht ist `docs/upstream.md`.
- **Windows:** `nvlddmkm` 153 nach Messbauten zählen (`docs/hardware.md`); 0 seit 30.10. 22:23.
- **Entscheidungen, die nur hier stehen:** Menüleiste über Vollbild darf dunkel satter/hell
  blasser wirken; fremde App-Symbole behalten ihre Form; Apples Icons und „macOS-Look“-Sets
  tabu (VISION); Kontrollzentrum-Pille grau 34 %, Menüleiste weiß 12 %, Menü-Glas 80 %;
  Animationswerte von 0.5.131 (27.11., „viel besser“), Alt+Tab-Ring 0,1 s, Schließen 0,2 s (28.11.: „gut wie es war“).
