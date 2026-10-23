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
privilegiertes Protokoll **`beize-shell-v1`** (Fensterliste mit App-ID, aktivieren,
minimieren, Blur hinter einer Fläche, Ursprung von Animationen). Beides bekommt nur die
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
  Streifen. Eigene Zeigerbilder für die Ränder kommen mit den Symbolen (d).
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
  maximiert, Zeiger auf einem Knopf), wie KWin seine Rahmen als Texturen hält. Die Leiste liegt
  über dem Inhalt. Platziert, maximiert und unter der Menüleiste gehalten wird der ganze
  Rahmen. GTK mit libadwaita zeichnet immer selbst. Das lässt sich nicht ändern und ist bei
  GNOME genauso.
- **Für alle Fenster** zeichnet der Compositor runde Ecken (Radius 10 wie macOS, Maske über
  die Abstandsfunktion im Shader wie KWin und Mutter) und den Schatten, je Pixel geschlossen
  berechnet (Evan Wallaces Formel, wie Zed und Figma), also ohne Textur je Fenstergröße. Das
  aktive Fenster hat den größeren, dunkleren Schatten. Ohne beides bleiben maximierte Fenster
  und Apps mit eigenem Rahmen und Schatten (GTK), wie bei GNOME. Später kommen die Animationen
  beim Öffnen, Schließen und Minimieren dazu. Ein minimiertes Fenster fliegt in sein Dock-Symbol, und von dort
  kommt es zurück.
- **Snap:** Ziehen an einen Bildschirmrand oder Super+Pfeiltaste gibt die halbe Fläche bzw.
  maximiert (Windows). Ein Doppelklick auf die Titelleiste maximiert. Snap-Layouts wie bei
  Windows 11 kommen später.
- **Fensterknöpfe** (`os/beize-draw`, `titlebar`), ein Code für Beize UI und den Rahmen des
  Compositors. Das Aussehen ist ein `Look`, heute fest `Mac`, später eine Einstellung:
  - **Mac (Vorgabe):** Die Leiste ist 28 px hoch. Links liegen drei Kreise (12 px, alle 20 px),
    Schließen rot, Minimieren gelb, Zoom grün. Bei inaktivem Fenster sind sie grau. Ist der
    Zeiger über ihnen, zeigen alle drei ihr Zeichen, gedrückt werden sie dunkler.
  - **Windows:** Die Leiste ist 32 px hoch, rechts flache Knöpfe 46 × 32, Schließen rot unter
    dem Zeiger.
  - Die Zeichen sind eigene Formen, für ihre Größe auf das Pixelraster gezeichnet, wie bei
    AppKit und Windows. Aus Phosphor wäre das Quadrat größer als das X.
  - Minimieren kommt mit dem Dock (c), bis dahin ist der gelbe Kreis grau, wie macOS einen
    nicht verfügbaren Knopf zeigt. Grün maximiert (Zoom). Vollbild und die Wahltaste
    (Zoom statt Vollbild) kommen mit echtem Vollbild.

### 3. Menüleiste (oben)

Durchgehend, so hoch wie macOS' Leiste (24 px bei Faktor 1), aus Grundglas (siehe 6).

- **Links:** das Beize-Menü (Über diesen Rechner, Einstellungen, Ruhezustand, Neustart,
  Ausschalten, Sperren, Abmelden; wie das Apple-Menü), dann der Name der aktiven App, fett.
- **App-Menüs:** Beize-Apps beschreiben ihre Menüs in Beize UI, und die Shell zeigt sie
  (Protokoll `beize-menu-v1`, wie AppKit das Menü an den WindowServer gibt). Fremde Apps
  liefern unter Wayland keine Menüs (GTK 4 hat das globale Menü entfernt, Qt kann es nur
  über KDEs D-Bus-Menü). Für sie zeigt die Leiste nur ein festes Menü: Name, Fenster der App,
  Beenden. Die Apps behalten ihre eigenen Menüs im Fenster. Das ist ehrlich gesagt der eine
  Punkt, an dem Beize nicht wie macOS sein kann.
- **Rechts:** Statussymbole (Netz, später Ton, Bluetooth, Akku), Suche, Kontrollzentrum,
  Datum und Uhrzeit. Bis es regionale Formate gibt (ADR 0017: LANG ist C.UTF-8), steht dort
  nur die Uhrzeit mit 24 Stunden. Sie springt zu Beginn jeder Minute um, auch wenn die Uhr
  gestellt wird (timerfd mit `CANCEL_ON_SET`). Ein Klick auf die Uhr öffnet später die Benachrichtigungen.

### 4. Dock (unten)

Schwebend, mittig, mit runden Ecken, aus Grundglas: angeheftete Apps, dann laufende Apps mit
Punkt darunter, rechts der Papierkorb (kommt mit dem Dateimanager).

- **Vorgaben wie macOS:** keine Vergrößerung beim Überfahren, nicht automatisch
  ausblenden. Beides wird später in den Einstellungen abschaltbar.
- **Ein Klick** startet die App bzw. holt ihre Fenster nach vorn. Gestartet wird wie das
  Terminal heute, als `app-<App-ID>@<Zufall>.service` über den Manager.
- **App-Identität:** `xdg_toplevel.app_id` ↔ Desktop-Datei (freedesktop) für fremde Apps.
  Beizes App-Bundles (VISION, vor M4) werden dieselbe ID tragen.
- Zum Start angeheftet: Terminal. Weitere Apps kommen, sobald es sie gibt.

### 5. Kontrollzentrum

Ein Glas-Panel unter der Menüleiste mit Kacheln wie bei macOS. Zuerst baut Schritt 4 den
Rahmen und die Kacheln, deren Dienst es schon gibt:

- **Netz** (networkd über Varlink `io.systemd.Network`, mit `os/beize-varlink`; WLAN mit
  iwd kommt mit #17).
- **Fokus** und **Hell/Dunkel**, sobald es Einstellungen gibt.
- **Ton, Bluetooth, Helligkeit, Medien** mit PipeWire und BlueZ (#17, M4).

Leere Kacheln für noch fehlende Dienste gibt es nicht. Die Shell zeigt nur, was
funktioniert.

### 6. Material, Animation, Hintergrund

Die drei Stufen der VISION, umgesetzt im Renderer des Compositors:

- **Grundglas** (Menüleiste, Dock, Titelleisten): Das Hintergrundbild wird einmal pro
  Wechsel weichgezeichnet und dann nur noch gelesen (Mica). Das kostet so gut wie nichts.
- **Lebendiges Glas** (Kontrollzentrum, Menüs, später Suche): Dual-Kawase auf ¼–⅛
  Auflösung, nur neu gerechnet, wenn sich dahinter etwas ändert. Ist das Budget knapp,
  sinkt die Auflösung, statt dass ein Bild verloren geht.
- **Fläche:** getönt mit feinem Korn, ohne Blur.
- Dazu Farbanhebung, eine Lichtkante von 1 px, Korn.
- **Start aus einem Guss:** Der Compositor zeigt das erste Bild erst, wenn die Shell
  Menüleiste und Dock geliefert hat (höchstens 1 s, dann ohne sie), und blendet es vom
  Schwarz des Modus-Wechsels ein. Es gibt kein Bild mit halbem Desktop, wie bei macOS
  erscheint der Schreibtisch ganz. Stürzt die Shell ab, bleibt das letzte Bild ihrer Flächen
  stehen, bis die neue Shell ihre liefert. Der flackerfreie Übergang von der Konsole (Fedoras
  „flicker-free boot“, CLAUDE.md §11) kommt mit der grafischen Anmeldung.
- **Animationen** sind Federn (gedämpfte Schwingung, unterbrechbar, wie SwiftUI). Sie
  laufen im Compositor im Takt der Bildwiederholung; jede hat einen Ursprung (Fenster aus
  dem Dock-Symbol). Schritt 4 legt das Gerüst und kurze Überblendungen an, die gestalteten
  Animationen (Minimieren ins Dock, Öffnen aus dem Symbol) kommen danach.
- **Hintergrund:** ein eigenes Standardbild als Verlauf, im Shader berechnet (keine
  Bilddatei im Image). Eigene Bilder kommen mit den Einstellungen.

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
Werkzeugen verwendet), einmal je Größe, und landen im Atlas wie die Glyphen. vello_cpu (ADR 0027
§9) bleibt außen vor, solange es eine 0.x-Version mit experimentellem Cache ist. Fremde
App-Symbole kommen aus dem hicolor-Thema (PNG oder SVG).

- **Symbole in Leiste, Menüs und Kontrollzentrum: Phosphor** (MIT, über 1500 Symbole). Es ist
  das offene Set, das SF Symbols am nächsten kommt: ein Stil in mehreren Strichstärken
  (passend zu Inters Gewichten) und je eine gefüllte Variante für „an“. Lucide und Tabler
  haben nur eine Strichstärke. Im Image liegen nur die Symbole, die Beize verwendet, als SVG
  aus dem offiziellen Release mit fester Prüfsumme (wie die Schriften): Phosphor veröffentlicht
  auf npm (`@phosphor-icons/core`), `bz outdated` fragt dort. Gerastert werden sie mit 16 px,
  wo Phosphors Strich genau 1 px ist. Am 23.10. erneut gegen Lucide und Tabler abgewogen:
  Phosphor bleibt, weil nur es jede Stärke eigens zeichnet und gefüllte und zweifarbige
  Varianten für alle Symbole hat, wie SF Symbols.
- **App-Symbole von Beize** (Terminal, später Dateien, Einstellungen): eigene SVGs, wenige,
  in einer gemeinsamen Form mit Verlauf. Fertige „macOS-Look“-Sets (z. B. WhiteSur) kopieren
  Apples Optik; das schließt die VISION aus (Konzepte, nicht Optik).

### 9. Lasttest „der Compositor soll sich langweilen“

Eine feste Testszene auf dem Test-Medium, nicht im Image: viele schwebende Fenster mit
bewegtem Verlauf, lebendiges Glas darüber, Schatten, runde Ecken, laufende Federanimationen,
durch den echten Compositor. Auf der Hardware wird sie mit `pacing:` gemessen. Ziel: kein
spätes Bild bei 240 Hz und eine Shell-Komposition unter 1 ms GPU-Zeit. In der CI (lavapipe)
prüft sie nur die Funktion, nicht die Zeit.

## Reihenfolge

a) `beize-shell` als Dienst, der Compositor erkennt sie, wlr-layer-shell nur für sie; eine
   Menüleiste mit Uhr, die Fenster darunter. ✓ 23.10.
b) Schwebende Fenster, Verschieben und Größe, Fokus; Titelleiste in Beize UI mit Phosphor
   (✓ 23.10.); xdg-decoration mit Rahmen für fremde Apps, `beize-draw` (✓ 23.10.); Knöpfe wie
   macOS, runde Ecken und Schatten (✓ 23.10.).
c) `beize-shell-v1` mit der Fensterliste; Dock: angeheftet, laufend, starten, aktivieren,
   minimieren; der Name der aktiven App in der Menüleiste.
d) Grundglas, Hintergrund, Symbole, der Start aus einem Guss; danach der Lasttest und ein
   Hardware-Test.
e) Kontrollzentrum mit der Netz-Kachel, lebendiges Glas, Beize-Menü (Sperren, Abmelden,
   Neustart, Ausschalten über logind).
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
