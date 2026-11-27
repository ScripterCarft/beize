# Stand und offene Punkte

Was gerade gilt. Verlauf steht in den Commits (`git log`), Entscheidungen in den ADRs,
Messwerte der Hardware-Tests in `~/hw-logs/results-*`. Hier wird ersetzt, nicht angehängt.

## Stand (27.11.2026)

- **M3 Compositor:** Desktop mit Shell (ADR 0028 (a)–(f)) läuft auf dem PC des Besitzers
  (RTX 3070 Ti, NVK, 2560×1440 @ 240 Hz). `main` ist 0.6, der letzte Messbau 0.5.131.
- **Animationen (ADR 0032) fertig und auf Hardware:** Schritte a)–e) gebaut, 0.5.130 und
  0.5.131 auf dem PC des Besitzers. Danach behoben: Trichter zurück aus dem Dock so schnell wie
  hinein (0,34 s statt 0,93 s), Schatten erst nach der Landung; die Probewerte von 0.5.131
  (Besitzer: „viel besser“) sind auf `main` (Tabelle in ADR 0032); Dialoge ruckelten beim
  Erscheinen (Inhalt und Glas verschieden gerundet, Glas jedes Bild neu angelegt) – behoben,
  noch nicht auf Hardware; Einrasten reicht 48 px vom Rand (mutter), der Alt+Tab-Ring schwingt
  nicht mehr nach. **Schließen war auf Hardware nie animiert** (kein „closes“ in den Journalen
  von 0.5.130/131): Beize-UI-Apps rissen beim Beenden die Verbindung ab; `App::drop` baut jetzt
  die Fenster ab wie `close` (a716d2d), der Smoke zählt es beim Terminal.
- **Anmeldedialog (ADR 0031):** auf Hardware gut, auch 48 s offen (0.5.131). run0 gab nach 25 s
  auf: eigener systemd-Patch 0002 (kein Timeout, solange polkit fragt; PID 1 und run0), für das
  offene Issue systemd#44507. Caps Lock geklärt: Taste bis Bild unter 1 ms; aus geht es beim
  Loslassen (xkb, wie GTK) – kein Fehler von Beize.
- **Fokus (ADR 0030):** neue Fenster kommen mit der Tastatur nach vorne wie bei Mutter; hält
  Beize die Tastatur (Dialog, Alt+Tab, Shell-Menü), öffnet ein neues hinten.
- **1Password läuft auf Beize noch nicht:** kein Weg, Apps zu installieren, und Electrons
  Bibliotheken (GTK3, NSS u. a.) fehlen.
- **Updates 27.11.:** Linux 7.3.6, Mesa 26.3.1, LLVM 23.1.6, Vulkan 1.4.369, linux-firmware
  20261124, neun Basis-Pakete (Sicherheitsfixes), Rust-Locks; neue Signaturschlüssel für
  tpm2-tss, glib, Mesa (je eigener Commit). xkeyboard-config bleibt 2.48 (2.49 ohne signiertes
  Archiv, #20). Grafik und Kernel noch nicht auf Hardware.
- **Upstream:** Stand aller Patches und Berichte in `docs/upstream.md` (27.11. geprüft): nouveau
  GSP-IRQDEST ist in Mainline (7.4, Cc stable); systemd #44842 braucht Nacharbeit (Bot-Review).
- **CI:** schneller Lauf je Push, nachts voll. Push braucht entsperrtes 1Password.

## Als Nächstes (der nächste Chat, in dieser Reihenfolge)

1. **Messbau 0.5.132** auf dem PC des Besitzers: neuer Grafik-Stack und Kernel 7.3 (späte
   Bilder gegen 0.5.131, Startzeiten aus dem binären Journal für #11), Schließen jetzt animiert,
   Dialoge ohne Ruckeln?, Probewert Alt+Tab-Ring 0,1 s (Besitzer: „schnell, aber nicht aus dem
   Nichts“; `TRIAL 0.5.132` in `switcher.rs`). Ergebnis auswerten, Werte nur nach seinem Ja.
2. **systemd-Entwürfe für main** (in WSL `~/scratch/sd-main`, Zweige `run0-timeout` und
   `logind-display`): kompilieren, logind v2 mit Test (TEST-35, zwei Sitzungen), nach
   `~/upstream/` als Patches; dann trägt Beize genau die gesendete Fassung. Stand und was vor dem
   Senden zu tun ist: `docs/upstream.md`.
3. **Design-Audit:** Screenshots selbst machen (`./bz debug … '@screendump build/x.png'`, alle
   Zustände: Desktop hell/dunkel, Menüs, Kontrollzentrum, Dock-Menü, Dialoge, Alt+Tab, Vorschau,
   Terminal; dazu die des Besitzers aus `~/hw-logs/results-*/*.png`), dann den Agenten
   `beize-design-auditor` (`.claude/agents/`) darauf ansetzen: belegte Funde zu Radien,
   Abständen, Schrift, Farben, Einheitlichkeit, Bewegung (gegen die Tabelle in ADR 0032).
   Ergebnis dem Besitzer mit Empfehlung vorlegen; Werte ändern sich nur nach seinem Ja.
4. Danach **Töne**, dann **Mitteilungen** – je ein ADR vorab.
5. Optimierung (Besitzer: erst die Shell fertig). Bekannt aus 0.5.130/131: erstes Bild jeder
   Animation spät (neues `Picture`/`LiveGlass` je Öffnen, `animation.rs`, `scene.rs:207`) – Pool
   nach Größe; Menü schließt: Bild im selben Frame gezeichnet (`state.rs` ~1884).

## Offen, mit Ort

- **Fragen an den Besitzer:** Schließen 0,2 s oder 0,15 s – erst beurteilbar mit 0.5.132, wo
  es zum ersten Mal auf Hardware animiert. Alt+Tab-Ring: Probewert 0,1 s in 0.5.132.
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
  Animationswerte von 0.5.131 (27.11., „viel besser“).
