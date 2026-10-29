# 0028 – Shell: Menüleiste, Dock, Fenster, Kontrollzentrum, Material

Status: Angenommen · 2026-10-23 (mit dem Besitzer besprochen)

## Kontext

Schritt 4 aus ADR 0027: Bisher füllt jedes Fenster den Bildschirm, ohne Titelleiste, und es
gibt keine Shell. Die VISION verlangt eine Shell aus einem Guss: Taskleiste, Kontrollzentrum,
Suche, Fenster mit Snap und Alt+Tab, Glas in drei Stufen, Federanimationen, Schatten,
Ecken und Animationen auch für fremde Apps. Das Budget lautet: Shell-Komposition unter
1 ms GPU-Zeit bei 1440p @ 240 Hz.

Was es schon gibt: Compositor auf Smithay mit eigenem Vulkan-Renderer und eigener
Bildschleife, Beize UI (Fenster, Text, Eingabe, Zwischenablage), das Terminal und den Start
von Apps als `app-<App-ID>@.service` (ADR 0027 §6–9). Netzwerk: networkd und resolved
(ADR 0011), Ton und Bluetooth kommen erst mit #17 bzw. M4.

Vom Besitzer entschieden (23.10.):

- **Titelleiste:** Beize UI zeichnet sie, wie AppKit; die Shell ergänzt (siehe 2).
- **Tastenkürzel:** Strg wie Windows und GNOME, nicht Cmd wie macOS. Grund: fremde Apps
  (Brave, Rider, GTK, Qt) sprechen Strg, so ist jedes Programm gleich, ohne Übersetzung.
- **Dock unten und Menüleiste oben, wie macOS.** Die Leiste trägt rechts den Status (Uhr,
  Netz, Ton, Akku) und das Kontrollzentrum, links das Beize-Menü und die aktive App.
- **Shell als eigener Prozess** wie bei macOS, und trotzdem aus einem Guss: Der Start bis in
  die Oberfläche läuft ohne Flackern (Abschnitt 6). Aufwendige Animationen kommen später.
- **Fensterknöpfe wie macOS** (23.10. geändert, vorher rechts): drei Kreise links. Das
  Aussehen der Leiste ist austauschbar (`Look`), ein Windows-Aussehen bleibt wählbar.
- **Symbole aus einem guten fertigen Set**, das an macOS erinnert, keine wochenlange eigene
  Zeichnung; App-Symbole selbst gezeichnet (Abschnitt 8).

## Entscheidung

### 1. Die Shell ist ein eigener Prozess mit Beize UI

`os/beize-shell` zeichnet Menüleiste, Dock, Kontrollzentrum und später Suche und
Benachrichtigungen, als Client mit Beize UI. Der Compositor behält, was nur er kann:
Fenster anordnen, Fokus, Eingabe, Effekte (Blur, Schatten, Ecken, Fensteranimationen) und
die Bildschleife.

Das ändert die Formulierung „Compositor = Shell“ aus ADR 0027 §1. Gründe:

- **Vorbild macOS:** Dock, Menüleiste und Kontrollzentrum sind dort eigene Prozesse
  (`Dock`, `SystemUIServer`, `ControlCenter`) neben dem WindowServer. Stürzt das Dock ab,
  startet es neu, und die Fenster bleiben. KDE (plasmashell neben KWin) und COSMIC (Panel
  neben cosmic-comp) trennen ebenso; nur GNOME Shell läuft im Compositor.
- **Ein Toolkit:** Text, Layout, Widgets und Barrierefreiheit (AccessKit) gibt es nur
  einmal, in Beize UI. Im Compositor bräuchte es eine zweite Widget-Schicht.
- **Sicherheit und Bildschleife:** Der Compositor bleibt klein. Zeichnet die Shell zu lange,
  verpasst nur ihre eigene Fläche ein Bild, nicht der ganze Bildschirm.

Die Shell spricht mit dem Compositor über **wlr-layer-shell** (Ebenen oben und unten,
reservierte Ränder; Smithay bringt es mit, COSMIC, KDE und niri nutzen es) und ein eigenes
privilegiertes Protokoll **`beize-shell-v1`** (`os/beize-protocols`, nach dem Vorbild von
ext-foreign-toplevel-list und wlr-foreign-toplevel-management): die Fensterliste mit App-ID,
Titel und Zustand (aktiv, minimiert), das Aktivieren, das ein minimiertes Fenster
zurückholt, das Erscheinungsbild (hell, dunkel) und Glas unter einer Fläche
(`beize_glass_v1`, Abschnitt 6); der Ursprung von Animationen kommt mit (g).
Minimiert wird vom Fenster aus (`xdg_toplevel.set_minimized`). Beize UI hält die Verbindung
einer App mit allen ihren Fenstern (`App`, wie NSApplication), so sind Menüleiste und Dock zwei
Fenster eines Prozesses auf einer Verbindung. Beides bekommt nur die
Shell; andere Clients sehen die Globals gar nicht. Der Compositor erkennt sie an ihrer Unit,
so wie xdg-desktop-portal eine App an ihrer `app-<ID>@`-Unit erkennt: Der Kernel nennt zur
Verbindung den Prozess als pidfd (`SO_PEERPIDFD`), dieser seine cgroup (`PIDFD_GET_INFO`),
und die muss die von `beize-shell.service` neben dem Compositor sein. Das ist ohne Wettlauf
mit einer wiederverwendeten PID und braucht kein `/proc`, das die Sandbox des Compositors
verbirgt. Einen Prozess in diese cgroup verschieben können nur Programme des Nutzers ohne
Sandbox, und die könnten die Shell ohnehin steuern; Apps in einer Sandbox können es nicht.
Die Shell läuft als `beize-shell.service` in `session.slice`; `beize-session.target` will
sie, ohne an sie gebunden zu sein. Nach einem Absturz startet sie neu, und die Fenster
bleiben. Der Compositor startet sie nicht selbst, sie wäre sonst ein Kind in seiner Sandbox
(wie das Terminal, ADR 0027 §9).

### 2. Fenster und Titelleiste

- **Fenster schweben:** Neue Fenster öffnen in ihrer gewünschten Größe (die Fläche unter der
  Leiste erfahren sie als `bounds`), mittig in dieser Fläche. Liegt dort schon ein Fenster,
  rückt das neue um eine Titelleistenhöhe (32 px) nach rechts unten, wie macOS' Kaskade.
  Nichts füllt mehr von selbst den Bildschirm. Fenster bleiben unter der Menüleiste und
  mindestens 64 px auf dem Bildschirm, damit man sie wieder greifen kann. Ein neuer Ort kommt
  immer mit dem Commit der neuen Größe, nie vorher (kein Bild mit altem Inhalt am neuen Ort).
- **Ränder:** Beize UI zeichnet keinen Rahmen. Darum gilt ein 5 px breiter Streifen außerhalb
  jedes Fensters als sein Rand, an dem der Compositor die Größe ändert, wie der WindowServer
  bei macOS. Apps mit eigenem Rand (GTK) behalten ihn, denn ihr Schatten liegt über dem
  Streifen; dort zeigt der Zeiger den Doppelpfeil (siehe 8).
- **Titelleiste von Beize UI** (Beize UI nutzt xdg-decoration nicht und zeichnet damit nach
  dem Protokoll selbst). Damit sind
  Werkzeugleisten in der Titelleiste möglich, wie bei Finder und Safari. Verschieben und
  Größe ändern gehen über `xdg_toplevel.move/resize` an den Compositor, er führt aus (Snap,
  Ränder). Der Titel steht mittig (Inter 13 px halbfett, grau bei inaktivem Fenster). Die
  Knöpfe zeigt die Leiste je nach Aussehen (`titlebar::Look`, Fensterknöpfe unten).
  Die App zeichnet und hört nur ihren Inhalt darunter, wie AppKits Content View. Ein
  Doppelklick maximiert (400 ms, GNOMEs Vorgabe). Ein maximiertes Fenster löst sich erst
  nach 4 px Ziehen (Windows' `SM_CXDRAG`), damit ein Klick es nicht verschiebt.
- **Fremde Apps**, die über xdg-decoration `server_side` wollen oder die Wahl dem Compositor
  lassen (Qt, Chromium/Brave, Electron; KWin entscheidet ebenso), bekommen vom Compositor
  dieselbe Titelleiste. Text, Symbole, Szene und Titelleiste liegen dafür in `os/beize-draw`,
  das Beize UI und der Compositor teilen, wie `beize-vulkan`. Der Compositor zeichnet die
  Leiste wie Beize UI durch eine sRGB-Ansicht in ein eigenes Bild (Mischen in linearem Licht,
  ADR 0027 §9) und zeigt es 1:1, sonst sähe die Kantenglättung des Titels anders aus als in
  Beize UI. Neu gezeichnet wird sie nur, wenn sie sich ändert (Titel, Breite, aktiv,
  maximiert, Zeiger auf einem Knopf), wie KWin seine Rahmen als Texturen hält. Den Titeltext
  zeichnet der Compositor selbst, wie Weston (`xwayland/window-manager.c`, cairo/pango; im
  Quelltext geprüft); die Shell zu fragen (wie GNOMEs `mutter-x11-frames`) wurde am 26.10. verworfen: jede
  fremde Leiste hinge an einem zweiten Prozess, gewonnen wären ~1,3 MB. Der Text-Stapel ist
  kleiner als Westons (pango, harfbuzz, freetype, fontconfig, fribidi: ~3 MB). Die Leiste liegt
  über dem Inhalt. Platziert, maximiert und unter der Menüleiste gehalten wird der ganze
  Rahmen. GTK mit libadwaita zeichnet immer selbst. Das lässt sich nicht ändern und ist bei
  GNOME genauso.
- **Für alle Fenster** zeichnet der Compositor runde Ecken (Radius 16 wie macOS 27, Maske über
  die Abstandsfunktion im Shader wie KWin und Mutter) und den Schatten, je Pixel geschlossen
  berechnet (Evan Wallaces Formel, wie Zed und Figma), also ohne Textur je Fenstergröße. Das
  aktive Fenster hat den größeren, dunkleren Schatten. Ohne beides bleiben maximierte Fenster
  und Apps mit eigenem Rahmen und Schatten (GTK), wie bei GNOME. Später kommen die Animationen
  beim Öffnen, Schließen und Minimieren dazu. Ein minimiertes Fenster fliegt in sein Dock-Symbol, und von dort
  kommt es zurück.
- **Snap:** Ziehen an einen Bildschirmrand oder Super+Pfeiltaste gibt die halbe Fläche bzw.
  maximiert (Windows). Ein Fenster stoppt an der Menüleiste; zieht man es mit dem Zeiger bis an
  den oberen Rand, füllt es die Fläche zwischen Menüleiste und Dock, wie seit macOS 15
  („Fenster an die Menüleiste ziehen, um den Bildschirm zu füllen“; Besitzer, 24.10.). Ein
  Doppelklick auf die Titelleiste maximiert. Snap-Layouts wie bei Windows 11 kommen später.
- **Echtes Vollbild** (Besitzer, 24.10., wie macOS; als eigener Schritt nach der Menüleiste):
  Fremde Apps fordern es an (F11 im Browser, Videos, Spiele; `xdg_toplevel.set_fullscreen`)
  und müssen es wirklich bekommen: das ganze Bild, über Menüleiste und Dock hinweg, die dann
  verschwinden. Am oberen Rand erscheint die Menüleiste unter dem Zeiger, dann deckend, weil kein
  Schreibtisch dahinter liegt (macOS seit Big Sur), am unteren das Dock. Super+F schaltet
  Vollbild für jedes Fenster. Eigene Spaces kommen später; bis dahin tritt ein Vollbild zurück,
  wenn eine andere App nach vorn kommt: Es bleibt Vollbild hinter ihr, Menüleiste und Dock sind
  wieder da (wie bei Windows); nach vorn geholt ist es sofort wieder Vollbild (Besitzer, 24.10.).
  Liegt ein Vollbild unter der Menüleiste, vorn oder zurückgetreten, liegt sie auf lebendigem
  Glas wie das Dock, das sich dem Vollbild darunter anpasst (Besitzer, 24.10.: einheitlich mit
  dem Dock statt macOS' deckender Leiste), auch statt des Grundglases der Einstellung unten; die
  Schrift folgt dann dem Erscheinungsbild, ohne Messung. Eingeblendet verdecken Leiste und Dock das Vollbild, sie
  verschieben es nicht (wie macOS): Die App behält ihre Größe, kein Neuzeichnen und kein Springen
  bei Video und Spiel. Die Leisten behalten ihre Streifen,
  die übrigen Fenster rücken nicht. Im Vollbild hat ein Fenster keinen Rahmen und keine
  Titelleiste (auch Beize UI nicht, wie AppKit); dass sie mit der Menüleiste erscheint wie bei
  macOS, kommt später. Noch nicht gebaut: Direct Scanout des Vollbild-Puffers (wie KWin, Mutter).
- **Kein Glas in Titelleisten** (Besitzer, 24.10.): Titelleisten sind von App zu App zu
  verschieden. Wichtig ist, dass jedes Programm die drei Knöpfe hat, wo es geht (Beize UI, der
  Rahmen des Compositors; libadwaita zeichnet eigene). Werkzeugleisten mit Knöpfen wie
  „Zurück“ kommen mit den eigenen Apps.
- **Fensterknöpfe** (`os/beize-draw`, `titlebar`), ein Code für Beize UI und den Rahmen des
  Compositors. Das Aussehen ist ein `Look`, heute fest `Mac`, später eine Einstellung:
  - **Mac (Vorgabe):** Links liegen drei Kreise, 14 px alle 23 px (an macOS 26 gemessen: 13,3/23;
    Besitzer 25.10.), die Leiste ist mit 9 px darüber und darunter 32 px hoch, der Titel 15 px.
    Schließen rot, Minimieren gelb, Zoom grün. Bei inaktivem Fenster sind sie grau. Ist der
    Zeiger über ihnen, zeigen alle drei ihr Zeichen, gedrückt werden sie dunkler.
  - **Windows:** Die Leiste ist 32 px hoch, rechts flache Knöpfe 46 × 32, Schließen rot unter
    dem Zeiger.
  - Die Zeichen sind eigene Formen, für ihre Größe auf das Pixelraster gezeichnet, wie bei
    AppKit und Windows. Aus Phosphor wäre das Quadrat größer als das X. Kreise und Zeichen
    rechnet der Quad-Shader je Pixel als Abstandsfelder (Kreis, Strich, Dreieck; 26.10.), wie
    schon die runden Ecken: scharf in jeder Größe, ohne SVG und ohne Platz im Atlas. Zoom
    bleibt zwei Dreiecke im 45°-Winkel (Besitzer).
  - Gelb minimiert: Das Fenster verlässt den Bildschirm, das darunter wird aktiv, und das Dock
    holt es an seinen Platz zurück. Grün maximiert (Zoom), mit Alt (der Wahltaste) schaltet es
    Vollbild (Besitzer, 24.10.: umgekehrt zu macOS).

### 3. Menüleiste (oben)

Durchgehend, 32 px hoch wie die Titelleisten, Schrift 15 px, **ohne Hintergrund** wie seit
macOS 26 (Besitzer, 24.10.): nur ihr Inhalt über dem Hintergrund. Unter ihr liegt nie ein
Fenster (sie hält ihren Streifen frei), also nur Beizes Hintergrund: Der Compositor misst ihn
dort einmal je Wechsel und sagt der Shell, ob es hell oder dunkel ist
(`beize_shell_v1.menu_bar_backdrop`). Wie bei macOS ist die Schrift fast immer weiß (Besitzer,
an seinem Mac beobachtet; Apple nennt keine Regel): Ein schwacher dunkler Verlauf nur innerhalb
der Leiste – schwarz 15 % am Bildschirmrand, bis zu ihrer Unterkante auf null auslaufend, also
ohne Kante; „du siehst ihn kaum, aber er ist da“ (Besitzer; Werte geschätzt) –, und weiß bleibt
die Schrift bis zu einem Kontrast von 2:1 (mittlere relative Luminanz der Leiste samt Verlauf
≤ 0,475; bewusst unter WCAGs 3:1, wie macOS); nur auf sehr hellem Hintergrund wird sie dunkel. Kein Schatten an der Schrift (Besitzer).

**Einstellung „Transparente Menüleiste“** (Besitzer, 24.10.: beide gefallen ihm; kommt mit den
Einstellungen, Vorgabe an): aus heißt die Leiste auf Grundglas, genau wie am 24.10. gebaut
(Commit c93184b: Grundglas über die ganze Leiste ohne Lichtkante, Schrift nach dem
Erscheinungsbild, dunkel auf hellem Glas); an heißt ohne Hintergrund wie oben. Modular: die
Shell bestellt Glas für die Leiste (`beize_glass_v1`) oder nicht; hat die Leiste Glas, folgt
die Schrift dem Erscheinungsbild statt dem gemessenen Ton, und der Verlauf darunter wird nie
gezeichnet (das Glas ist deckend, Smithays Schadensverfolgung überspringt Verdecktes). Bis zu
den Einstellungen schaltet `BEIZE_MENU_BAR=glass` in `~/.config/environment.d` (gilt ab der
nächsten Anmeldung); geprüft am Bild in der VM. Die Smoke-Prüfung beider Fälle kommt mit der
Einstellung, die dann zur Laufzeit umschaltet.

- **Links:** das Beize-Menü (Über diesen Rechner, Einstellungen, Ruhezustand, Neustart,
  Ausschalten, Sperren, Abmelden; wie das Apple-Menü), dann der Name der aktiven App, fett.
  Gebaut 28.10. (e): Beizes Zeichen (Besitzer: zwei schräge Balken mit runden Enden wie ein B,
  als zwei Striche gezeichnet) öffnet es; heute Ruhezustand, Neustart, Ausschalten, Abmelden über
  logind auf dem System-Bus (polkit: die aktive lokale Sitzung darf, wie GNOME). Sperren kommt mit
  dem Sperrbildschirm, „Über diesen Rechner“ und Einstellungen mit ihren Fenstern. Sie handeln
  sofort; macOS' Rückfrage („Restart…“) kommt mit Dialogen. Abmelden beendet die Sitzung von
  beize-session (`XDG_SESSION_ID`). Lücke in logind: Für die Shell, die neben der Sitzung läuft,
  nimmt polkit die Display-Sitzung des Benutzers; logind wählt sie nur beim Start einer Sitzung,
  nicht bei `SetType` – war der Benutzer vorher anders angemeldet (serielle Konsole, SSH), fragt
  polkit nach dem Passwort. Patch für upstream vorbereitet (`~/upstream/systemd-logind-display`,
  der Besitzer schickt ihn); mit der grafischen Anmeldung entsteht die Sitzung ohnehin als
  Wayland-Sitzung (wie bei GDM).
- **Menüs** (`beize_ui::menu`, ein Baustein auch für Kontext- und App-Menüs): ein Popup
  (`xdg_popup` mit Grab: ein Klick daneben schließt es, wie anvil und niri, und wirkt zugleich
  wie ohne Menü – eine Titelleiste zieht, ein Rand ändert die Größe, das Fenster kommt nach vorn
  (Besitzer, 28.10.; der Compositor lässt den Zeiger vor dem Klick los, sonst lief er durch
  Smithays Grab ohne Klick-Grab, und das Verschieben wurde abgelehnt); danach bekommt das
  aktive Fenster die Tastatur zurück) auf Menü-Glas – lebendig, mit macOS' Menüfarben #F2F2F2 bzw.
  #2D2D2D zu 85 % getönt (Besitzer: ruhig, kaum durchscheinend; macOS 26 lässt etwa die Hälfte
  durch), schwache Lichtkante, Schatten nur außen. Maße aus den Menübildern der HIG (genau 2 px
  je pt) × 1,25: Zeilen 30, oben und unten 6, Text 15 px (wie die Leiste) mit 20 Einzug,
  Trennlinie 1 px schwarz 7,5 %, Ecken 16 (wie Fenster), mindestens 225 breit. Geschätzt: die
  Hervorhebung (Akzentblau, 6 eingerückt, Ecken 8), der Schatten. Gewählt wird beim Loslassen
  über einem Eintrag oder mit Return, Escape oder ein Klick in der Shell schließt. Der Knopf eines
  offenen Menüs liegt auf einer Pille mit runden Enden, 28 px hoch: beim Beize-Menü 45 breit
  (macOS 26: 36 pt), beim Kontrollzentrum 54 (Besitzer 28.10.; Apples 46,5 × 24 pt × 1,25 füllte
  die Leiste). Der Knopf des Kontrollzentrums ist seine Pille: Uhr und Globus halten 8 px Abstand
  von ihr (Apple ~6 pt bis zum nächsten Symbol). Die Leiste hat 20 px an den Enden (Besitzer;
  macOS 12 pt × 1,25 = 15), der Globus ist 20 px groß (Kugel ~16).
- **App-Menüs:** Beize-Apps beschreiben ihre Menüs in Beize UI, und die Shell zeigt sie
  (Protokoll `beize-menu-v1`, wie AppKit das Menü an den WindowServer gibt). Fremde Apps
  liefern unter Wayland keine Menüs (GTK 4 hat das globale Menü entfernt, Qt kann es nur
  über KDEs D-Bus-Menü). Für sie zeigt die Leiste nur ein festes Menü: Name, Fenster der App,
  Beenden. Die Apps behalten ihre eigenen Menüs im Fenster. Das ist ehrlich gesagt der eine
  Punkt, an dem Beize nicht wie macOS sein kann.
- **Rechts:** Statussymbole (Netz – Phosphors `globe-simple`, ohne Verbindung `globe-simple-x`,
  Besitzer 28.10. –, später Ton, Bluetooth, Akku), Suche, Kontrollzentrum,
  Datum und Uhrzeit wie macOS („Mon 9 Jun 07:36“, 24 Stunden), die Namen von der C-Bibliothek:
  englisch, bis es Sprachen gibt (ADR 0017; Besitzer 25.10.). Sie springt zu Beginn jeder Minute um, auch wenn die Uhr
  gestellt wird (timerfd mit `CANCEL_ON_SET`). Ein Klick auf die Uhr öffnet später die Benachrichtigungen.

### 4. Dock (unten)

Schwebend, mittig, mit runden Ecken, aus Grundglas: angeheftete Apps, dann laufende Apps mit
Punkt darunter, rechts der Papierkorb (kommt mit dem Dateimanager).

- **Vorgaben wie macOS:** keine Vergrößerung beim Überfahren, nicht automatisch
  ausblenden. Beides wird später in den Einstellungen abschaltbar.
- **Ein Klick** startet die App bzw. holt ihre Fenster nach vorn, das zuletzt benutzte
  obenauf; sind alle minimiert, kommt das zuletzt minimierte zurück (wie macOS' Dock).
  Gestartet wird als `app-<App-ID>@<Zufall>.service` über den Manager (`beize-systemd`,
  `apps`), von der Shell selbst, wie Strg+Alt+T vom Compositor. Fremde Apps ohne eigene
  Unit-Vorlage (nur `Exec=`) kommen mit ihnen.
- **App-Identität:** `xdg_toplevel.app_id` ↔ Desktop-Datei (freedesktop); ihr `Name` steht
  in der Menüleiste, ohne Desktop-Datei die ID. Beizes App-Bundles (VISION, vor M4) werden
  dieselbe ID tragen.
- **Ecken wie Apples:** stetig gekrümmt (die Kurve der Maske aus Apples App-Icon-Vorlage) für
  Fenster, Dock und App-Symbole, alle aus einer Abstandsfunktion im Shader; Kreisbögen nur, wo
  die Kurve nicht passt (Punkte, Kapseln). Maße des Docks an Apples Bildern gemessen, für
  Beize größer (Besitzer, 25.10.; `beize-draw` `measures`): 90 hoch, Symbole 56 px dicht
  nebeneinander, ringsum 17 px zum Rand, der Punkt schwarz mittig darunter.
- Beizes Apps zeigen ihr Symbol (siehe 8), andere ihr Symbol aus hicolor, eine App ohne Symbol
  eine Kachel mit ihrem Anfangsbuchstaben. Das Dock liegt auf Glas mit Lichtkante (siehe 6),
  der Punkt laufender Apps ist dunkel auf hellem, hell auf dunklem Glas.
- Zum Start angeheftet: Terminal. Weitere Apps kommen, sobald es sie gibt.

### 5. Kontrollzentrum

Kacheln aus klarem Glas (§6) unter dem rechten Ende der Menüleiste, **ohne Panel**: jede
schwebt für sich, dahinter scheint verschwommen durch, was dort liegt, wie das Kontrollzentrum
von macOS 26 (Apples Pressebild bestätigt es; Besitzer 28.10.). Um die Kacheln liegt Rauch
(§6): auch zwischen und neben ihnen ist, was dahinter liegt, verwischt, ab ihrem Rand nach außen
weich auslaufend über 130 px, bis unter die Leiste – ein Fenster dahinter macht sie nicht
schwer lesbar (Besitzer 29.10., an Mock-ups; Apples Pressebild zeigt es so). Kein Schatten.
Ein Popup der Leiste; ein Klick daneben schließt es, und daneben fällt er durch (Eingabe-Region
nur auf den Kacheln; das Popup wächst nicht mit dem Rauch).

- **Raster** gemessen an Apples Pressebild (3840 px, ~3,94 px je pt über die Versalhöhe der
  Leiste): vier Spalten zu 64 pt mit 12 pt Abstand, 292 pt breit – das Bild bestätigt die Summe –,
  in Beize × 1,25 und 6,5 % kleiner (Besitzer 28.10., alles in der Kachel mit): Kachel 75,
  Abstand 14, 14 unter der Leiste; rechts bündig mit dem Ende der Uhr (20 px Rand der Leiste).
  Eine Zelle ist ein Kreis, 2 × 1 eine Kapsel, 2 × 2 hat Ecken von 21 pt (25), eine Reihe aus
  vier (Regler) 16,5 pt (19). Runde Knöpfe in Kapseln 36 pt (42), „an“ ist eine weiße Scheibe mit
  dem Symbol in Akzentblau. Titel der Kapsel 10 % leichter als halbfett (540), 2 px über der
  Zeile darunter; das Symbol von Hell/Dunkel 30 px (Besitzer).
- **Anordnung** (Besitzer, 28.10.): Netz oben links (2 × 1), daneben der Mediaplayer (2 × 2),
  darunter Bluetooth (2 × 1), darunter Hell/Dunkel (1 × 1), dann Helligkeit und Lautstärke je
  über die ganze Breite. Gebaut 28.10.: **Netz** (networkd über Varlink
  `io.systemd.Network.Describe`, das jeder fragen darf; Änderungen über inotify auf networkds
  Zustandsdateien, wie sd-networks Monitor, das libsystemd nicht öffentlich anbietet; virtuelle
  Geräte zählen nicht) und **Hell/Dunkel** (schaltet zur Laufzeit, §6). Bluetooth, Medien und
  Lautstärke mit PipeWire und BlueZ (#17, M4), Helligkeit mit ihnen; Fokus mit den
  Einstellungen.
- Leere Kacheln für noch fehlende Dienste gibt es nicht: Reihen ohne Kachel rücken zusammen,
  und das Kontrollzentrum ist so breit wie die belegten Spalten, also rechts unter seinem Knopf.
- Die Schrift auf dem Glas ist in beiden Erscheinungsbildern weiß, die Zeile darunter zu 70 %;
  das klare Glas hält dafür das Licht dahinter unten. Über hellem Inhalt bleibt es graues
  Rauchglas, die Schrift springt nicht um (Besitzer 29.10.).

### 6. Material, Animation, Hintergrund

Die drei Stufen der VISION, umgesetzt im Renderer des Compositors. Das Material zeichnet
der Compositor, nicht die App, wie macOS' WindowServer und Windows' DWM: Die Shell sagt über
`beize_glass_v1`, wo Glas liegt (Rechteck der Fläche, Radius, Lichtkante ja/nein, das Material,
Schatten, Rauch; gilt mit ihrem nächsten Commit), und zeichnet darauf nur ihren Inhalt.

- **Grundglas** (heute keine Fläche; gerechnet erst, wenn eine Fläche es will): Der
  Hintergrund wird einmal pro Wechsel
  (Bildschirmgröße, Erscheinungsbild) weichgezeichnet – Dual Kawase wie bei KWin, vier
  Halbierungen und zwei Verdopplungen zurück, das Glas hat ¼ der Größe –, seine Sättigung
  ×1,8 angehoben und getönt: Hell `#F6F6F6` zu 30 %, Dunkel `#1E1E20` zu 45 % (Anhalt:
  Mica mit `#F3F3F3` zu 50 % und `#202020` zu 80 %; geschätzt, vom Besitzer am 24.10. an
  Entwürfen und Bildschirmfotos abgenommen). Danach wird es nur noch gelesen (Mica). Ein Bild
  fügt je Pixel nur Korn (±1,5 Stufen) und die Lichtkante hinzu; das Glas gilt als deckend,
  darunter wird nichts gezeichnet. Hintergrund und Blur haben einen eigenen Shader
  (`backdrop.frag`), damit der Shader jedes Bildes klein bleibt.
- **Lebendiges Glas** (Dock – Besitzer, 24.10.: Fenster dahinter scheinen verschwommen durch
  wie bei macOS –, Menüleiste über Vollbild, Kontrollzentrum, Menüs, später Suche): was dahinter liegt – Fenster, ihre
  Schatten, der Hintergrund –, im selben Durchgang wie das Bild. Der Compositor zeichnet dafür die
  Elemente hinter dem Glas (das Glas und 64 px darum, für den Rand des Blurs) in ein eigenes
  kleines Bild mit eigener Schadensverfolgung, nie aus dem Bildschirmpuffer, in dem das Glas
  selbst schon liegt. Nur wenn sich dort etwas geändert hat, läuft derselbe Blur wie beim
  Grundglas, mit Anhebung und Tönung, so sehen beide über dem Hintergrund allein gleich aus.
  Sonst kostet es ein Bild nichts als das Lesen. Ist das Budget knapp, sinkt die Auflösung,
  statt dass ein Bild verloren geht (mit dem Lasttest messen).
- **Fläche:** getönt mit feinem Korn, ohne Blur.
- **Lichtkante:** ein Ring von etwa 1,5 px innen am Rand. Ihre Helligkeit folgt der Richtung
  zum Licht (oben links), schwächer auf der Gegenseite, dazwischen fast keine – wie Apples
  Liquid Glass; je Pixel aus der Abstandsfunktion der Ecken, kostet praktisch nichts.
  Gleichmäßig rundum ist ein Schalter im Shader (`RIM_FOLLOWS_LIGHT`), damit man es später
  vergleichen kann (Besitzer). Lichtbrechung am Rand wie bei Liquid Glass hat bisher nur das
  klare Glas (Linse, unten).
- **Erscheinungsbild:** Hell oder Dunkel ist eine Systemeinstellung, nicht eine Eigenschaft
  des Bildes; Vorgabe Hell wie macOS. Das Glas nimmt die Tönung dazu, der Hintergrund seine
  Fassung, die Shell ihre Schrift (der Compositor sagt es ihr über `beize-shell-v1`). Die
  Kachel im Kontrollzentrum schaltet es zur Laufzeit (`beize_shell_v1.set_appearance`); der
  Compositor merkt sich die Wahl bis zu den Einstellungen in `~/.config/beize/appearance` (seine
  `ConfigurationDirectory`, wie GNOME sein Farbschema in dconf). `BEIZE_APPEARANCE=dark` oder
  `light` in `~/.config/environment.d` geht vor (Tests, Bilder). Apps' eigene Fenster folgen
  noch nicht.
- **Menü-Glas** (`beize_glass_v1.material` `menu`, wie NSVisualEffectView `.menu`): lebendiges
  Glas, dicht getönt (§3, Menüs), mit schwacher Lichtkante; Glas kann Schatten werfen
  (`set_shadow`) und an mehreren Stellen einer Fläche liegen (`add_shape`, die Kacheln).
- **Klares Glas** (`material` `clear`, die Kacheln des Kontrollzentrums; Werte am 29.10. an
  Mock-ups abgenommen): lebendig, aber nur Gauß σ 6 px, keine Tönung; auf den kodierten Werten
  Sättigung ×1,5 und die Helligkeit gestaucht, `L/(1+aL)` mit `a = 1/Decke − 1` – Dunkles bleibt,
  Weiß kommt bei der Decke an (0,52 hell, 0,50 dunkel: weiße Schrift hat über Weiß noch Kontrast
  3,7 bzw. 4,0); linear gerechnet käme Weiß bei ~0,75 an. Dazu je Pixel im Bild: eine Linse
  (innerhalb 10 px vom Rand zeigt es, was entlang der Normalen weiter außen liegt, um bis 9 px,
  `(1 − innen/10)² · 9`), weißer Glanz 5 % oben bis 0 bei 35 % der Höhe, innerer Schein 16 % über
  4 px und die Lichtkante 0,7, beide zum Licht oben links hin heller; kein Korn. Bis es einmal
  unscharf ist, zeigt es nichts statt des scharfen Hintergrunds.
- **Rauch** (`set_smoke(falloff, strength)`): was hinter den Formen einer Fläche und um sie liegt,
  verwischt – ab dem Rand jeder Form nach außen `t = (1 − d/falloff)^1,6`, σ aus t über die
  Stufen 0; 0,075; 0,15; 0,3; 0,6; 1 × `strength` (Kontrollzentrum: 130 px, 4 px); nur
  Unschärfe, keine Tönung, kein Rand, kein Korn. Ein eigenes lebendiges Glas hinter der Leiste
  (deren Schrift bleibt scharf), vor Fenstern; die Maske t rechnet die CPU nur bei Änderung von
  Formen oder Lage (¼ Auflösung), dann zwei trennbare Gauß-Pässe mit σ je Pixel (wie die
  Tiefenunschärfe in Spielen; keine gemischten Kawase-Stufen, die Blöcke und Geisterkanten
  hinterlassen wie gestapelte CSS-`backdrop-filter`), nur wenn sich dahinter etwas ändert – sonst
  ein Textur-Quad je Bild. Ohne gültige Unschärfe zeichnet er nichts und deckt nichts; stürzt die
  Shell ab, bleiben Rauch und klares Glas nicht stehen.
- **Start aus einem Guss** (gebaut 24.10., `os/beize-compositor/src/opening.rs`): Beim
  Übernehmen des Bildschirms setzt der Compositor den Modus sofort mit einem schwarzen Bild, so
  rastet der Monitor ein, während die Shell startet und der Hintergrund dahinter entsteht. Den
  Desktop zeigt er erst, wenn die Shell `beize_shell_v1.ready` sagt (nach dem ersten Bild von
  Menüleiste und Dock), und blendet ihn aus dem Schwarz ein: Feder ohne Nachschwingen wie
  SwiftUIs `.smooth` (0,5 s). Es gibt kein Bild mit halbem Desktop, wie bei macOS erscheint der
  Schreibtisch ganz, der Zeiger mit ihm. Ohne die Shell nur, wenn ihre Verbindung vor `ready`
  endet (sofort) oder sie hängt (nach 10 s); die feste Zeit ist eine Übergangslösung, das
  Konzept dafür ist #40. Gemessen in der VM (lavapipe): warm 0,17 s, mit leerem Shader-Cache
  0,9–1,2 s, unter Last einmal 2,6 s. Stürzt die Shell ab, bleiben ihre Leisten so stehen, wie
  das letzte Bild sie zeigte, samt dem Platz, den sie Fenstern nehmen (kein Fenster rückt), bis
  eine neue Shell `ready` sagt (Neustart in der VM 0,2–0,5 s); eingefroren ändert sich ihr Glas
  nicht, Klicks gehen an die neue Shell. Der Desktop startet ohne Fenster, wie macOS (das
  Terminal öffnen Dock und Strg+Alt+T). Der flackerfreie Übergang von der Konsole (Fedoras
  „flicker-free boot“, ADR 0027 §6) kommt mit der grafischen Anmeldung.
- **Animationen** sind Federn (gedämpfte Schwingung, unterbrechbar, wie SwiftUI). Sie
  laufen im Compositor im Takt der Bildwiederholung; jede hat einen Ursprung (Fenster aus
  dem Dock-Symbol). Schritt 4 legt das Gerüst und kurze Überblendungen an, die gestalteten
  Animationen (Minimieren ins Dock, Öffnen aus dem Symbol) kommen danach.
- **Hintergrund:** ein eigenes Standardbild, im Shader berechnet (keine Bilddatei im
  Image), vom Besitzer am 24.10. an Entwürfen gewählt: vier Farbpunkte, in Oklab gemischt (so
  werden Übergänge nicht trüb), darüber vier geschwungene Bänder mit scharfer, heller
  Oberkante. Ohne sie wäre Grundglas unsichtbar: ein weicher Verlauf sieht weichgezeichnet
  aus wie vorher. Das oberste Band liegt unter der Menüleiste, damit ihre Kante sichtbar
  bleibt. Je eine helle und eine dunkle Fassung mit denselben Formen, wie macOS' dynamische
  Bilder; Dithering gegen Streifen. Lage nach der Bildschirmhöhe, so behält es auf jedem
  Seitenverhältnis seine Form. Eigene Bilder kommen mit den Einstellungen.

### 7. Tastenkürzel (Strg-Modell)

| Kürzel | Wirkung | Vorbild |
| --- | --- | --- |
| Strg+C/V/X/Z/S … | in den Apps wie gewohnt | Windows, GNOME |
| Strg+Shift+C/V | Kopieren/Einfügen im Terminal (Strg+C bricht ab) | GNOME Terminal |
| Super (allein) | Suche | Windows-Taste |
| Alt+Tab, Alt+Shift+Tab | zwischen Fenstern wechseln, mit Vorschau | Windows, GNOME |
| Super+Pfeiltasten | Snap, maximieren, wiederherstellen | Windows |
| Super+L | Sperren (mit dem Sperrbildschirm) | Windows, GNOME |
| Alt+F4 | Fenster schließen | Windows, GNOME |
| Strg+Alt+T | neues Terminal (bleibt) | Ubuntu |

Super gehört der Shell, keine App bekommt Super-Kürzel. Damit ist der Punkt aus ADR 0027 §9
erledigt: Strg+Shift+C/V im Terminal ist endgültig.

### 8. Symbole

Vektorsymbole (SVG) werden mit **resvg** gerastert (Rust, ausgereift, in Typst und vielen
Werkzeugen verwendet), einmal je Größe, und landen im Atlas wie die Glyphen; farbige Bilder
(App-Symbole) in einem eigenen RGBA-Atlas, wie Skia farbige Glyphen von den 8-Bit-Glyphen
trennt. vello_cpu (ADR 0027 §9) bleibt außen vor, solange es eine 0.x-Version mit
experimentellem Cache ist. Fremde App-Symbole kommen aus dem hicolor-Thema (PNG oder SVG,
gewählt nach der Icon Theme Specification). Beides nur in Beize UI und seinen Apps
(`beize-draw`, Feature `icons`): Der Compositor zeichnet weder Symbole noch App-Symbole und
wird ohne resvg gebaut (26.10.: 7569 → 6517 KiB).

- **Zeiger: Bibata Modern Classic** (GPL-3.0, Xcursor; im Image nur 24 px und 48 px für
  Faktor 2, das Release hat 14 Größen), vom Besitzer am 24.10. gewählt
  vor Adwaita, Breeze, Capitaine und Phinger: schwarz mit weißem Rand wie macOS' Zeiger, ohne sie
  zu kopieren; Warten animiert (54 Bilder je 40 ms), „Warten“ und „verboten“ farbig. Eingetragen
  als Thema „default“ für Apps, die Zeiger selbst laden. Der Compositor zeigt ihn mit 24 px:
  benannt von Apps über cursor-shape-v1 (Beize UI: Text-Strich über dem Inhalt des Terminals,
  sonst der Pfeil), wo keine App ist, den Pfeil oder an einem Fensterrand den Doppelpfeil. Der
  Rand des vorderen Fensters gewinnt vor den Fenstern dahinter, wie bei macOS. Ein eigener
  Zeigersatz oder ein besserer ist für später vorgemerkt (Besitzer).

- **Symbole in Leiste, Menüs und Kontrollzentrum: Phosphor** (MIT, über 1500 Symbole). Es ist
  das offene Set, das SF Symbols am nächsten kommt: ein Stil in mehreren Strichstärken
  (passend zu Inters Gewichten) und je eine gefüllte Variante für „an“. Lucide und Tabler
  haben nur eine Strichstärke. Im Image liegen nur die Symbole, die Beize verwendet, als SVG
  aus dem offiziellen Release mit fester Prüfsumme (wie die Schriften): Phosphor veröffentlicht
  auf npm (`@phosphor-icons/core`), `bz outdated` fragt dort. Gerastert werden sie mit 16 px,
  wo Phosphors Strich genau 1 px ist. Am 23.10. erneut gegen Lucide und Tabler abgewogen:
  Phosphor bleibt, weil nur es jede Stärke eigens zeichnet und gefüllte und zweifarbige
  Varianten für alle Symbole hat, wie SF Symbols.
- **App-Symbole von Beize** (Terminal, später Dateien, Einstellungen): eine gemeinsame Form,
  wie macOS' Symbole sie teilen – das Quadrat des Icon-Rasters mit den stetigen Ecken (siehe 4),
  vom Shader mit einem Verlauf in den Farben der App gefüllt –, darauf ein Phosphor-Symbol als
  Motiv in Weiß (`beize-draw`, `icon`; Terminal: `terminal` fett auf Graphit, vom Besitzer am
  24.10. abgenommen). Keine Bilddatei je App. Fertige „macOS-Look“-Sets (z. B. WhiteSur) kopieren
  Apples Optik; das schließt die VISION aus (Konzepte, nicht Optik).

### 9. Lasttest „der Compositor soll sich langweilen“

Eine feste Testszene auf dem Test-Medium, nicht im Image: viele schwebende Fenster mit
bewegtem Verlauf, lebendiges Glas darüber, Schatten, runde Ecken, laufende Federanimationen,
durch den echten Compositor. Auf der Hardware wird sie mit `pacing:` gemessen. Ziel: kein
spätes Bild bei 240 Hz und eine Shell-Komposition unter 1 ms GPU-Zeit. In der CI (lavapipe)
prüft sie nur die Funktion, nicht die Zeit.

Gebaut 24.10.: `beize-load-test` (Beize UI, also Puffer wie bei jeder eigenen App: dmabufs auf
der GPU) öffnet 12 Fenster zu 640×400, die der Compositor kaskadiert; jedes zeichnet jedes Bild
neu (Verlauf mit wanderndem Farbton, vier durchscheinende runde Blöcke, eine Zeile Text mit der
Bildnummer). Fenster setzt eine App nicht selbst: Unter das Dock kommt eins, wie bei macOS, nur
durch Ziehen (Smoke über das Tablet, auf der Hardware der Besitzer). Der Smoke (volle Stufe)
verlangt: jedes Fenster ≥ 2 Bilder/s über 6 s, das Glas des Docks ändert sich mit dem Fenster
dahinter (Gegenprobe ohne Fenster dahinter: rot), eine `pacing:`-Zeile über die Szene. In der
VM 12 × ~33 Bilder/s, lavapipe weit über dem Budget (erwartet). Federanimationen des
Compositors gibt es bis (g) nur beim Start; sie kommen in die Szene, sobald es sie gibt.
Erste Messung auf der 3070 Ti (0.5.102, 24.10.): 6568 von 7200 Bildern in 30 s, 542 spät (479
„display“: fertig nur 0,1–0,7 ms vor dem vblank), GPU-Median 0,32 ms – das Ziel unter 1 ms hält –,
aber 1,44 ms CPU je Bild: der Compositor langweilt sich noch nicht. `pacing:` zeigt seitdem, wohin
die CPU-Zeit geht (Szene, Glas, Bild, Commit).

## Reihenfolge

a) `beize-shell` als Dienst, der Compositor erkennt sie, wlr-layer-shell nur für sie; eine
   Menüleiste mit Uhr, die Fenster darunter. ✓ 23.10.
b) Schwebende Fenster, Verschieben und Größe, Fokus; Titelleiste in Beize UI mit Phosphor
   (✓ 23.10.); xdg-decoration mit Rahmen für fremde Apps, `beize-draw` (✓ 23.10.); Knöpfe wie
   macOS, runde Ecken und Schatten (✓ 23.10.).
c) `beize-shell-v1` mit der Fensterliste; Dock: angeheftet, laufend, starten, aktivieren,
   minimieren; der Name der aktiven App in der Menüleiste. ✓ 23.10.
d) Symbole, Hintergrund, lebendiges Glas für das Dock, die Menüleiste ohne Hintergrund
   (✓ 24.10.), echtes Vollbild mit lebendigem Glas unter der Leiste (✓ 24.10.), der Start aus
   einem Guss; danach der Lasttest und ein
   Hardware-Test.
e) Kontrollzentrum mit der Netz-Kachel, lebendiges Glas, Beize-Menü (Sperren, Abmelden,
   Neustart, Ausschalten über logind). ✓ 28.10. (ohne Sperren: kommt mit dem Sperrbildschirm;
   dazu Hell/Dunkel zur Laufzeit und Menüs als Baustein in Beize UI)
f) App-Menüs (`beize-menu-v1`), Alt+Tab, Snap.
g) Gestaltete Federanimationen (Öffnen, Minimieren ins Dock, Snap).

Suche, Benachrichtigungen, Sperrbildschirm und die grafische Anmeldung bekommen je ein
eigenes ADR.

## Konsequenzen

- ADR 0027 §1 ist angepasst: Compositor und Shell sind getrennte Prozesse aus einer Hand.
- Ein eigenes privilegiertes Protokoll plus wlr-layer-shell. Der Compositor muss prüfen,
  welcher Client die Shell ist.
- Fremde Apps bekommen Rahmen, Ecken, Schatten und Animationen, aber keine Einträge in der
  Menüleiste.
- Neuer Code: Shell, Fensterverwaltung, Effekte im Renderer. Neue Fremd-Crates: resvg (mit
  usvg und tiny-skia); neue Daten: Phosphor-Symbole.
