# Stand und offene Punkte

Was gerade gilt. Verlauf steht in den Commits (`git log`), Entscheidungen in den ADRs,
Messwerte der Hardware-Tests in `~/hw-logs/results-*`. Hier wird ersetzt, nicht angehängt.

## Stand (17.11.2026)

- **M3 Compositor:** Desktop mit Shell (ADR 0028 (a)–(f)) läuft auf dem PC des Besitzers
  (RTX 3070 Ti, NVK, 2560×1440 @ 240 Hz). `main` ist 0.6, der letzte Messbau 0.5.128.
- **Fokus (ADR 0030, 15.11.):** neue Fenster kommen mit der Tastatur nach vorne wie bei Mutter
  (1Passwords Linux-App ist darauf gebaut); vorhandene brauchen ein Token; hält Beize die
  Tastatur (Dialog, Alt+Tab, Shell-Menü), öffnet ein neues hinten. `come-forward` und der
  Startzähler sind weg. 0.5.128 auf Hardware: ein Fenster von außen kommt beim Tippen nach vorne
  und bekommt die Tasten, bei offenem Shell-Menü öffnet es hinten ohne sie. Alt+Tab war nicht
  prüfbar (mit einem Fenster geht der Umschalter nicht auf; der Smoke-Test prüft es).
- **Anmeldedialog (ADR 0031):** 0.5.129 auf Hardware gut. 17.11. danach: Beim Prüfen bleiben die
  Punkte gedimmt im Feld (ohne Rahmen/Caret), es leert sich erst mit dem Urteil; die alte
  Fehlerzeile verschwindet beim Absenden, das Urteil erscheint neu. Tastatur-LEDs folgen jetzt
  Caps/Num Lock (fehlte ganz). Noch nicht auf Hardware. Die Shell ist polkits
  Agent für die Display-Sitzung. run0 im Desktop-Terminal und grafische Apps fragen im Dialog:
  Schloss (Fluent Emoji 3D) mit dem Symbol der App, Passwortfeld, die Knöpfe nebeneinander.
  Der Bestätigungsdialog hat die Knöpfe auch nebeneinander. logind wählt die Display-Sitzung
  nach `SetType` neu: eigener Patch in `recipes/patches/systemd` (bz trägt Rezept-Patches wie
  die Kernel-Patches). Damit fällt auch die „known“-Ausnahme im Menü-Test weg.
- **1Password läuft auf Beize noch nicht:** kein Weg, Apps zu installieren, und Electrons
  Bibliotheken (GTK3, NSS u. a.) fehlen. Der echte Test (`op`, SSH-Agent, Browser) kommt damit.
- **Updates 15.11.:** expat 2.10.0, zlib 1.4, tpm2-tss 4.2.2 (Sicherheit), systemd 262.2,
  openssl 4.1.0 und kleinere; neue Signaturschlüssel iproute2 (Unterschlüssel), tpm2-tss
  (Moritz Buhl), wayland-protocols (Daniel Stone). xkeyboard-config 2.49 ohne signiertes Archiv:
  bleibt 2.48.
- **0.5.127 auf Hardware:** Vorschau gehalten 0,6–0,85 ms GPU, 0 späte Bilder; nur die ersten
  ~1,5 s bis 4,9 ms (GPU-Takt, erschlossen). Taste bis Bild 2–6 ms. Verdeckte Fenster 1 Bild/s
  (gewollt).
- **CI:** schneller Lauf je Push, nachts voll; vom 11. bis 14.11. rot (rustix-Meldung, expat,
  zlib-Signatur weg), mit den Updates behoben. Push braucht entsperrtes 1Password.

## Als Nächstes

1. **Animationen nach ADR 0032** (angenommen 17.11., Entwürfe
   https://claude.ai/artifact/UUATm6qxfcen8KgjNbvDPB, danach von einem Agenten geprüft und
   eingearbeitet). Schritt a) ist gebaut: Feder in `beize-draw` (`spring`), Startblende darauf,
   ausgewertet an `Gpu::aim`, Ende ohne Bildschirm. Schritt b) ist gebaut (17.11.): eigene Bilder des
   Compositors im Bild-Submit (`animation.rs`, `render::frame::Canvas`), Fenster öffnen am Ort
   und schließen, Testfaktor `BEIZE_ANIMATIONS_SLOWER`, Journalzeilen, Smoke-Szenario
   „Animationen“ mit Gegenproben; 18.11. die Dialoge der Shell: erscheinen, schließen, und das
   Schütteln bei falschem Passwort (`beize_shell_v1.dialog`/`shake`, Glas mit Deckkraft im
   Shader). Schritt c) ist gebaut (18.11.): Minimieren als Trichter ins Dock-Symbol und zurück
   (`genie.rs`, Gitter `STRIP` im Renderer, `beize_shell_v1.dock_icon`), Start aus dem Dock
   mit Token der Shell in einer transienten Unit aus der Desktop-Datei (`beize-systemd`
   `apps`/`desktop`), das Fenster wächst aus dem Symbol; die Template-Unit des Terminals ist
   weg. Noch nicht auf Hardware; dort ansehen, ob das Bild im Hals flimmert (keine Mipmaps,
   ADR 0032 §5). In d) ist Einrasten und Maximieren gebaut (18.11.): Rahmen federt ab dem Commit
   der neuen Größe, das alte Bild blendet über; Menüs blenden beim Schließen aus (Bild und
   Glas-Geist); das Kontrollzentrum wächst aus seinem Knopf (`beize_shell_v1.grows`, Liquid-Glas
   blendet); der Alt+Tab-Ring federt in der Shell. Damit ist d) fertig; als Nächstes e) der
   Messbau mit allem (braucht den Besitzer).
   Werte: SwiftUI-Federn + Regeln in §6, kein Mac-Messen (Besitzer). Apple-Optik bei Bewegung
   erwünscht, wo sauber (Trichter), keine Spielerei.
2. Danach **Töne**, dann **Mitteilungen** – je ein ADR vorab.
   **Nebenbefund:** Einmal beobachtet (17.11., `bz smoke --only …` direkt nach einem Bau): Der erste Start der
   Installation wartete vergeblich auf `/dev/gpt-auto-root-luks`, obwohl „Repartition Root
   Disk“ fertig war; beim nächsten Lauf lief sie durch. Konsole gesichert unter
   `build/work/install-hang-02-smoke.log`. Ursache noch nicht gefunden.
   **Nebenbefund 18.11. (Gate, einmal):** Im Szenario „Desktop“ zeigte das Terminal beim
   Bildvergleich nur „/r“ vom getippten Befehl (24059 Pixel daneben), direkt nach einem Bild mit
   2,8 s auf der GPU (VM). Der nächste Lauf war grün. Logs in `build/work/flake-1118/`. Geklärt
   (26.11.): Die zwei „opened“ gehören zu den zwei Terminals, die `session.rs` absichtlich öffnet
   (zwei „opens“, das zweite im Kaskadenschritt) – kein Fehler. Offen: wohin die Tasten nach „/r“
   gingen. 26.11.: Fokuswechsel ausgeschlossen (kein Klick, kein „activated“, kein Unmap im
   Journal; Animationen ändern den Fokus nicht). Nicht mehr zu entscheiden (Maschine überschrieben,
   nur 40 Journalzeilen; libinputs „too slow“ ist auf 5/h gedrosselt): verloren in QEMUs
   PS/2-Puffer (16 Bytes, verwirft bei VM-Stau die neuen) oder erst im Gast. Der Smoke zählt jetzt
   die IRQ-1-Bytes vor/nach dem Tippen (grün: 64 von 64); ein roter Lauf nennt sie. Beim nächsten
   Auftreten zuerst `build/work/logs/boot` ganz sichern.
3. Messbau mit allem: Dialog (Prüfzustand, Caps-LED), Caps-Lock-Messzeilen, Animationen.
   `pam_faildelay` bleibt 2 s (Besitzer 17.11.).
4. **Grafik-Updates mit Hardwaretest:** Kernel 7.3 (nouveau-Patches prüfen), Mesa 26.3, LLVM,
   Vulkan, linux-firmware.
5. Optimierung (Besitzer: erst die Shell fertig).

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
- **Smoke, Snap (16.11., einmal in `gate`):** Das Testclient-Fenster war beim Ziehen an den Rand
  weg, und es kam keine Vorschau, obwohl der Client den Druck bekam. Die VM war überlastet (GPU
  bis 80 ms, libinput „too slow“). Erschlossen: `xdg_toplevel.move` kam erst, als der Zeiger
  schon am Rand war. Das Benutzer-Journal dieses Starts fehlte; beim nächsten Auftreten mit
  Messzeilen im Compositor (Grab-Start, Zeiger dabei) belegen.
- **Anmeldedialog:** Auswahl zwischen mehreren Admins per Menü fehlt (ADR 0031; heute: der
  Benutzer selbst, sonst der erste genannte, mit Namen im Satz); der Cursor im Feld blinkt
  nicht; Schütteln kommt mit den Federanimationen. Caps Lock „verzögert“ (0.5.129): Einschalten
  geht denselben Weg wie ein getippter Buchstabe, Ausschalten kommt mit dem Loslassen (xkb, wie
  GTK) – im Messbau mit Zeitstempeln (Taste, Modifiers, Bild) belegen. Im Smoke nicht abgedeckt: `CancelAuthentication`
  und die Warteschlange; das Klicken ins Terminal wartet fest 300 ms statt auf den Fokus.
- **Apps installieren:** Beize hat keinen Weg dafür; Voraussetzung für 1Password und jede
  Drittanbieter-App. Eigene Entscheidung mit ADR, Zeitpunkt beim Besitzer.
- **Issues:** `gh issue list` (M3-Plan: #14, #17, #34, #35).

## Beim Besitzer

- **Upstream senden** (Entwürfe in WSL `~/upstream/`, Text schreibt der Besitzer):
  systemd run0-Ausgabe (`systemd-ptyfwd-drain/`),
  sbsigntools Monat (`sbsigntools-month-patch/`), Smithay-Berichte (`smithay/ISSUES.md`).
  Gesendet, warten: nouveau EDID (21.10.) und GSP-IRQDEST (23.10.) – beide in `kernel/patches/`,
  `bz` meldet, wenn ein neuer Kernel sie enthält; systemd logind (#44842, 16.11.), Beize trägt
  ihn in `recipes/patches/systemd`, `bz` meldet, wenn systemd ihn enthält; Smithay PR #2244 gemergt, `[patch]` fällt mit
  dem Release nach 0.7.0; ntpd-rs #2539.
- **Windows:** `nvlddmkm` 153 nach Messbauten zählen (`docs/hardware.md`); 0 seit 30.10. 22:23.
- **Entscheidungen, die nur hier stehen:** Menüleiste über Vollbild darf dunkel satter/hell
  blasser wirken; fremde App-Symbole behalten ihre Form; Apples Icons und „macOS-Look“-Sets
  tabu (VISION); Kontrollzentrum-Pille grau 34 %, Menüleiste weiß 12 %, Menü-Glas 80 %.
