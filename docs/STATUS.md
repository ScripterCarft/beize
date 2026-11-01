# Stand und offene Punkte

Was gerade gilt. Verlauf steht in den Commits (`git log`), Entscheidungen in den ADRs,
Messwerte der Hardware-Tests in `~/hw-logs/results-*`. Hier wird ersetzt, nicht angehängt.

## Stand (1.11.2026)

- **M3 Compositor:** Desktop mit Shell (ADR 0028 (a)–(f)) läuft auf dem PC des Besitzers
  (RTX 3070 Ti, NVK, 2560×1440 @ 240 Hz). `main` ist 0.6, der letzte Messbau 0.5.127.
- **Fehlersuche 1.11.** (fünf Prüf-Agenten, alles mit Smoke-Prüfung und Gegenprobe): Recovery-Key
  nach abgebrochenem ersten Start, Update nach Stromausfall und aus unbestätigtem Start,
  Start-Bestätigung samt Desktop (`beize-compositor-check`), Fokus und Menü-Grabs, Titel-/Menüflut,
  voller Socket in Beize UI, Bildfehler-Schleife, verdeckte Fenster ohne Wecker, Popup-Leck,
  `told`-Drossel im Terminal ersetzt, Protokollfehler, Null-Buffer. Werkzeuge: `bz check`/`gate`,
  Hooks, Cargo-Target bleibt, Smoke ohne VM-Takt, `--only`, `desktop/` geteilt, `ci/measure.sh`.
- **0.5.127 auf Hardware:** Vorschau gehalten 0,6–0,85 ms GPU, 0 späte Bilder; nur die ersten
  ~1,5 s bis 4,9 ms (GPU-Takt, erschlossen). Taste bis Bild 2–6 ms. Verdeckte Fenster 1 Bild/s
  (gewollt).
- **CI:** schneller Lauf je Push, nachts voll; zuletzt grün auf 7308929. Push braucht entsperrtes
  1Password.

## Als Nächstes

1. **Fokus neu (eigener Chat, Besitzer 1.11.):** keine Ausnahmeliste (`come-forward`) und kein
   Startzähler; eine strenge Regel mit Tokens für jeden Start durch das System, Passwortfeld
   stellt auch gültige Tokens zurück, Abgelehnte melden sich. Prüfstein des Besitzers: `op` im
   Terminal → Entsperrdialog von 1Password vorn mit Fokus im Feld, wie Windows
   (`AllowSetForegroundWindow`) und macOS (`yieldActivation`) – erst recherchieren (was macht
   1Password unter Linux), dann Vorschlag, dann bauen. Der Hinweis im Dock kommt später.
2. **Shell (g)** Federanimationen (ADR 0028), dann Optimierung (Besitzer: erst die Shell fertig).

## Offen, mit Ort

- **Dienste:** A7 TPM aus + SMBIOS-Kommandozeile → Recovery-Abfrage in verändertem Start
  (`beize-tpm/src/main.rs:83`; signierte Kommandozeile vergleichen); Keyring-Fehler zählt als
  „TPM entsperrt“ (`main.rs:149`); Recovery-Keys in beize-tpm als `String`; ein abgestürzter
  Compositor führt zurück in die angemeldete Konsole (vor dem Sperrbildschirm lösen).
- **Compositor:** Glas der Vorschau meldet sich als undurchsichtig, `take_shown` löst FIFO der
  Fenster dahinter (`backdrop.rs` `opaque_regions`); Aufräumen: `with_place`/`with_menus`/`with_bar`
  mit toten `None`-Zweigen, `popup_grab_ended` doppelt zu `active_window`, Zeiger-Begrenzung in
  `rescan` doppelt.
- **Terminal:** Schrift-Verzeichnis je Zelle neu gelesen (`beize-draw/src/text.rs:183`).
- **Vorschau:** die ersten 1,5 s nach dem Erscheinen – zwei Vorschauen nacheinander messen.
- **NVK:** Tabelle im Shader las Unsinn (Index belegt im Bereich) – Treiberfehler erschlossen,
  Reproduzierer auf Hardware fehlt; Kommentare in `quad.frag`, `backdrop.frag`, `quad.vert`.
- **Tests:** Desktop-Szenario in eigenständige Teile (parallele VMs); ein Journal-Leser statt
  ~170 Abfragen; Menü-Ereignisse statt fester Pausen.
- **VM:** #41 vCPU steht bis 21 s (verschachteltes KVM), Ursache offen.
- **Issues:** `gh issue list` (M3-Plan: #14, #17, #34, #35).

## Beim Besitzer

- **Upstream senden** (Entwürfe in WSL `~/upstream/`, Text schreibt der Besitzer):
  systemd run0-Ausgabe (`systemd-ptyfwd-drain/`), systemd logind (`systemd-logind-display/`),
  sbsigntools Monat (`sbsigntools-month-patch/`), Smithay-Berichte (`smithay/ISSUES.md`).
  Gesendet, warten: nouveau EDID (21.10.) und GSP-IRQDEST (23.10.) – beide in `kernel/patches/`,
  `bz` meldet, wenn ein neuer Kernel sie enthält; Smithay PR #2244 gemergt, `[patch]` fällt mit
  dem Release nach 0.7.0; ntpd-rs #2539.
- **Windows:** `nvlddmkm` 153 nach Messbauten zählen (`docs/hardware.md`); 0 seit 30.10. 22:23.
- **Entscheidungen, die nur hier stehen:** Menüleiste über Vollbild darf dunkel satter/hell
  blasser wirken; fremde App-Symbole behalten ihre Form; Apples Icons und „macOS-Look“-Sets
  tabu (VISION); Kontrollzentrum-Pille grau 34 %, Menüleiste weiß 12 %, Menü-Glas 80 %.
