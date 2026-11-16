# Stand und offene Punkte

Was gerade gilt. Verlauf steht in den Commits (`git log`), Entscheidungen in den ADRs,
Messwerte der Hardware-Tests in `~/hw-logs/results-*`. Hier wird ersetzt, nicht angehängt.

## Stand (16.11.2026)

- **M3 Compositor:** Desktop mit Shell (ADR 0028 (a)–(f)) läuft auf dem PC des Besitzers
  (RTX 3070 Ti, NVK, 2560×1440 @ 240 Hz). `main` ist 0.6, der letzte Messbau 0.5.128.
- **Fokus (ADR 0030, 15.11.):** neue Fenster kommen mit der Tastatur nach vorne wie bei Mutter
  (1Passwords Linux-App ist darauf gebaut); vorhandene brauchen ein Token; hält Beize die
  Tastatur (Dialog, Alt+Tab, Shell-Menü), öffnet ein neues hinten. `come-forward` und der
  Startzähler sind weg. 0.5.128 auf Hardware: ein Fenster von außen kommt beim Tippen nach vorne
  und bekommt die Tasten, bei offenem Shell-Menü öffnet es hinten ohne sie. Alt+Tab war nicht
  prüfbar (mit einem Fenster geht der Umschalter nicht auf; der Smoke-Test prüft es).
- **Anmeldedialog (ADR 0031, 16.11., Smoke grün, noch kein Messbau):** Die Shell ist polkits
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

1. **Messbau mit dem Anmeldedialog** für den PC des Besitzers (run0 im Terminal, Aussehen
   hell/dunkel, Caps Lock).
2. **Grafik-Updates mit Hardwaretest:** Kernel 7.3 (nouveau-Patches prüfen), Mesa 26.3, LLVM,
   Vulkan, linux-firmware.
3. **Shell (g)** Federanimationen (ADR 0028), dann Optimierung (Besitzer: erst die Shell fertig).

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
  nicht; Schütteln kommt mit den Federanimationen. Im Smoke nicht abgedeckt: `CancelAuthentication`
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
