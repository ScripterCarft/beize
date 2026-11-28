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
- **Späte Bilder seit 0.5.132 deutlich mehr** (Hardware, gleiche Handgriffe): 223 statt 81
  während Animationen, 317 statt 102 je Sitzung. GPU-spät vor allem bei „dialog closes“ 57 (0.5.131:
  10), „comes back“ 44 (5), „menu closes“ 52 (13), „dialog appears“ 38 (31); neu 92 „late (drawing)“
  (vorher 2), fast alle in der Phase `begin` von `render/mod.rs` `submit()` (bis 21 ms; dort
  laufen `dmabufs.retain`, `shared.collect()` = Freigaben, Uploads, `write_quads`) – beim
  Einrasten/Maximieren ~25 ms nach „moves“ und beim Live-Resize. Ursache nicht belegt; neuer
  Grafik-Stack und Kernel sind der einzige Unterschied zu 0.5.131.
- **Upstream:** `docs/upstream.md` (27.11. geprüft): nouveau GSP-IRQDEST in Mainline (7.4, Cc
  stable); systemd #44842 „needs-rework“, v2 und die run0-Fassung für main liegen in `~/upstream`.
- **1Password läuft auf Beize noch nicht:** kein Weg, Apps zu installieren.
- **CI:** schneller Lauf je Push, nachts voll; zuletzt alles grün. Push braucht entsperrtes 1Password.

## Als Nächstes: erst der Gesamtplan, dann bauen

Der Besitzer (28.11.) will keinen Problemen einzeln hinterherlaufen, sondern einen Stand, auf dem
Beize ohne Sorgen aufbaut. **Erster Auftrag des nächsten Chats: einen Plan vorlegen**, der die
Stränge unten verbindet – Reihenfolge so, dass nichts doppelt gebaut oder später wieder umgebaut
wird, mit Blick auf künftige Systeme. Leitfragen des Besitzers:
- Brauchen wir für ein Problem ohnehin bald ein professionelleres System? Dann jetzt umsteigen
  statt flicken.
- Ursachen im Code suchen und belegen, **nicht mit vielen Messbauten**; wo man lange lesen muss,
  umstrukturieren (Trennung der Zuständigkeiten, guter Stil).
- Was kann der Besitzer parallel tun, während die KI arbeitet (Entscheidungen, Mock-ups wählen,
  Upstream senden, Hardware-Tests bündeln)?
- Wie werden Bauen, Smoke und Tests schneller?

Die Stränge, mit dem, was schon bekannt ist:
1. **Späte Bilder** (Zahlen oben): Render-Pfad durchgehen – was in `submit()` vor dem Aufzeichnen
  passiert, gehört es in den Bildpfad (Freigaben/Importe aus dem Takt nehmen, wie Mutter/KWin es
  halten)?; bekannt: neues `Picture`/`LiveGlass` je Öffnen (`animation.rs`, `scene.rs:207`), Menü
  schließt mit Bild im selben Frame (`state.rs` ~1884), „flip event repeated the vblank before“
  häuft sich. Größte Dateien: `state.rs` 2719 Zeilen, `animation.rs` 1816, `render/mod.rs` 1719,
  `windows.rs` 1688, `beize-ui/app.rs` 1597, `beize-shell/main.rs` 1507.
2. **UI-Overhaul mit eigener Formensprache (neues ADR), Claude als Design-Direktor** (CLAUDE.md
  §2: entscheidet selbst, darf jedes Element ändern, Ziel „überwältigt“). Besitzer: Beize soll
  Eigenes haben, alles aus einem Guss; das Liquid Glass (Dock, Kacheln des Kontrollzentrums) ist
  „nichts Ganzes“, schwer, überladen; das **Menü-Glas** (80 % #F2F2F2/#2D2D2D über Unschärfe,
  `backdrop.rs` `menu_material`) ist „mega“. Ablauf: Bestandsaufnahme (eigene Screenshots aller
  Zustände über `bz debug … '@screendump'`, seine aus `~/hw-logs`), Agent
  `beize-design-auditor` plus eine UX-Prüfung der Abläufe → ADR mit Grundwerten (Abstände,
  Radien, Schrift, Farben, Bewegung) und **einem** Materialsystem → zwei, drei Richtungen als
  Mock-up mit dem echten Renderer, Claude entscheidet und zeigt sie dem Besitzer → schrittweise
  umbauen, jede Stufe vom Gutachter geprüft und vom Besitzer auf Hardware abgenommen. Frühere
  Abnahmen (Liste unten, ADR 0028/0032) sind Ausgangspunkt, keine Sperre.
3. **UX-Lücken:** Ein Dock-Klick holt ein minimiertes Fenster nur zurück, wenn alle Fenster der
  App minimiert sind (`dock.rs` `release`); sonst nur per Rechtsklick – macOS zeigt Minimiertes als
  eigene Kacheln, Windows 11 Vorschauen; Lösung mit Vorbild vorschlagen. Die Raute für
  minimierte Fenster im Dock-Menü (`Mark::Diamond`) wirkt fremd.
4. **Code und Tests:** Aufräumen (Liste „Compositor“ unten); Desktop-Szenario in eigenständige
  Teile mit parallelen VMs (heute 232 s + 124 s Animationen); ein Journal-Leser statt ~170
  Abfragen; Menü-Ereignisse statt fester Pausen.
5. **Künftige Systeme, die heute Entscheidungen beeinflussen:** Apps installieren (Paketformat,
  Sandbox, Portale – Voraussetzung für 1Password und jede Drittanbieter-App), Updates über das
  Internet (#15), Töne und Mitteilungen (je ADR; Mitteilungen brauchen die neue Formensprache).
  Klären, was davon Compositor, Shell oder Beize UI jetzt schon anders zuschneiden würde.
6. **systemd-Entwürfe** (`docs/upstream.md`): kompilieren, logind v2 mit Test, dann trägt Beize
  die gesendete Fassung.

## Offen, mit Ort

- **logind:** `session_stop()` wählt das Display neu und speichert nur die gehende Sitzung, nicht
  die neue (`logind-session.c` ~1033, systemd main) – derselbe Fall wie Punkt 1 des Bot-Reviews
  von #44842; prüfen, ob es nach einem logind-Neustart falsch wird, dann upstream.
- **Dienste:** A7 TPM aus + SMBIOS-Kommandozeile → Recovery-Abfrage in verändertem Start
  (`beize-tpm/src/main.rs:83`; signierte Kommandozeile vergleichen); Keyring-Fehler zählt als
  „TPM entsperrt“ (`main.rs:149`); Recovery-Keys in beize-tpm als `String`; ein abgestürzter
  Compositor führt zurück in die angemeldete Konsole (vor dem Sperrbildschirm lösen).
- **Compositor:** Glas der Vorschau meldet sich als undurchsichtig, `take_shown` löst FIFO der
  Fenster dahinter (`backdrop.rs` `opaque_regions`); Aufräumen: `with_place`/`with_menus`/`with_bar`
  mit toten `None`-Zweigen, `popup_grab_ended` doppelt zu `active_window`, Zeiger-Begrenzung in
  `rescan` doppelt. Beim Einrasten wird der Schatten mitgestreckt (`animation.rs` `stretched`);
  Popups eines Fensters liegen während des Trichters ungebogen am Ziel (`scene.rs` ~115);
  „flip event repeated the vblank before“ häuft sich bei Animationen – prüfen, ob es „late“
  verfälscht.
- **Terminal:** Schrift-Verzeichnis je Zelle neu gelesen (`beize-draw/src/text.rs:183`).
- **NVK:** Tabelle im Shader las Unsinn (Index belegt im Bereich) – Treiberfehler erschlossen,
  Reproduzierer auf Hardware fehlt; Kommentare in `quad.frag`, `backdrop.frag`, `quad.vert`.
- **Tests:** Desktop-Szenario in eigenständige Teile (parallele VMs); ein Journal-Leser statt
  ~170 Abfragen; Menü-Ereignisse statt fester Pausen. Der Dialog-Test wartet jetzt 26 s
  (das prüft polkits Timeout echt).
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
