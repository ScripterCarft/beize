# 0032 – Animationen: Federn im Compositor

Status: Angenommen · 2026-11-17 (Besitzer, an den Entwürfen:
https://claude.ai/artifact/UUATm6qxfcen8KgjNbvDPB; danach unabhängig geprüft, Befunde eingearbeitet)

## Kontext

Die VISION verlangt Animationen, die auf Federn beruhen, sich unterbrechen lassen und einen
Ursprung haben („Fenster wächst aus seinem Icon“). ADR 0028 §6 und (g) legen dafür nur den
Rahmen fest. Der Besitzer sagt dazu am 17.11.: „wirklich cleane Animationen, nichts darf
hängen, wie bei Apple sehr durchdacht“. Er will polierte Animationen, die man ausliefern kann,
ohne Spielerei: kein starkes Wackeln, kein starkes Nachfedern.

Heute gibt es genau eine Animation, das Einblenden des Desktops (`opening.rs`). Sie ist eine
kritisch gedämpfte Feder wie SwiftUIs `.smooth` und fordert ihr nächstes Bild beim
„frame done“ an. Ihr Wert wird aber zum Beginn des Zeichnens berechnet (`veil(began)`), nicht
für den vblank, an dem das Bild erscheint. Alles andere springt: Öffnen, Schließen,
Minimieren, Einrasten, Menüs, Kontrollzentrum und Dialoge.

Was belegt ist und was nicht:
- **Belegt (Apple-Dokumentation, WWDC23 „Animate with springs“):** `Spring(duration:bounce:)`
  hat die Masse 1, die Steifigkeit (2π/d)² und bei bounce ≥ 0 die Dämpfung 4π(1−b)/d. Apples
  Beispiel d = 0,5, b = 0,3 ergibt 157,9 und 17,6, und die Formel trifft das. Eine neue
  Animation übernimmt Ort und Geschwindigkeit der laufenden. Core Animation trennt die
  Modellebene (der Wert, der gilt) von der Darstellungsebene (das, was gerade zu sehen ist).
  `CADisplayLink.targetTimestamp` ist der Zeitpunkt, an dem das nächste Bild erscheint.
- **Belegt (Verhalten):** Der Autorisierungsdialog von macOS („*App* wants to make changes“)
  schüttelt sich bei einem falschen Passwort (Eclectic Light, 23.10.2024). KWin hat den
  Trichter als Effekt „Magic Lamp“: das Fenster als Gitter, dessen Punkte sich zum Symbol
  verbiegen (`magiclamp.cpp`, 250 ms linear). gnome-shell öffnet in 150 ms, minimiert in
  400 ms und klont beim Größenwechsel das alte Bild vorher (`windowManager.js`,
  `_sizeChangeWindow`). niri hält ein eigenes Bild eines Fensters fürs Schließen fest, im
  Pre-Commit des Null-Puffers (`store_unmap_snapshot`).
- **Nicht veröffentlicht:** Apples Werte für Fenster (macOS 26 animiert wieder das Öffnen und
  Schließen von Dokumenten), den Trichter, Menüs, das Kontrollzentrum und das Schütteln.
  Messen am Mac ist nicht möglich (Besitzer, 17.11.). Die Werte kommen deshalb aus SwiftUIs
  dokumentierten Federn und begründeter UX-Überlegung (Abschnitt 6).

## Entscheidung

### 1. Eine Feder für alles

`beize-draw` hat `spring` (gebaut 17.11.): SwiftUIs Feder mit (Dauer, Nachschwingen),
analytisch gelöst, mit Wert und Geschwindigkeit als Zustand. Ein Zeitschritt ist deshalb
exakt, wie lang er auch ist, und ein neues Ziel übernimmt Wert und Geschwindigkeit
(unterbrechbar wie in SwiftUI). Eine Bewegung endet an ihrem Ziel, sobald die Feder sich
nachweislich nie mehr weiter als „unsichtbar“ davon entfernt (Hüllkurve der Schwingung). Das
sind ½ Stufe bei 8 Bit für Deckkraft und ¼ physischer Pixel für Orte und Größen. Bei einer
Größe ohne Einheit (der Fortschritt des Trichters) ist es ¼ Pixel geteilt durch den längsten
Weg. Compositor und Beize UI nutzen dieselbe Feder (`deny.toml`: `beize-draw` liegt unter
beiden). Die Startblende läuft schon darauf.

Ausnahmen mit fester Dauer, jede mit Vorbild: das Abblenden im Leerlauf (linear über 10 s,
`idle.rs`, wie GNOME) und die Überblendung beim Einrasten. Die Überblendung ist eine eigene Feder,
die neben dem Rahmen läuft, keine Kurve.

### 2. Modell und Bild getrennt

Wie bei Core Animation gilt jede Änderung **sofort im Modell**: Ort, Größe, Stapel, Fokus,
Tastatur, `configure` an die App und das Treffen von Klicks. Die Animation ist nur das, was
der Compositor zeichnet. Daraus folgt:
- Keine Animation hält eine Taste, einen Klick oder eine Antwort an eine App auf. Ein Fenster,
  das gerade aufgeht, hat die Tastatur schon. Beim Minimieren hat das Fenster darunter den
  Fokus schon, bevor das Bild im Dock ankommt.
- Klicks treffen das Modell, nie das bewegte Bild (`surface_under` geht über den Space; Mutter
  und KWin halten es ebenso). Ein Fenster, das schließt oder minimiert, nimmt nichts mehr an.
- **Animiert werden nur Zustandswechsel:** Öffnen, Schließen, Minimieren, Maximieren und
  Einrasten. Beim Ziehen und beim Ändern der Größe folgt der Rahmen dem Zeiger 1:1, auch beim
  Abziehen aus maximiert oder eingerastet (Mutter animiert während eines Grabs nicht). Ein
  Fenster, das hinter dem Zeiger herfedert, hängt.
- Keine Animation wartet auf eine App.

**Eigene Bilder des Compositors.** Wo eine Animation ein Bild zeigt, das die App nicht mehr
liefert, macht der Compositor eine eigene Kopie: das fertige Fenster mit Inhalt, Titelleiste,
Ecken, Schatten und Glas, offscreen gezeichnet in eine Textur, die ihm gehört. Den Puffer
der App einfach länger zu halten, wäre falsch, denn nach der Freigabe darf die App ihn wieder
beschreiben (`syncobj.rs`). Vorbild ist niri. Die Kopie entsteht:
- **beim Schließen** im Pre-Commit-Hook, wenn ein Null-Puffer kommt (wie `timing.rs` und
  `syncobj.rs` ihn schon nutzen), oder in `toplevel_destroyed`, solange der letzte Puffer
  noch da ist; beim Zerstören eines Popups (Menü) ebenso, mit seinem Glas;
- **beim Einrasten und Maximieren** vor dem `configure` der neuen Größe (gnome-shell klont
  vorher), damit das alte Bild noch da ist, wenn das neue kommt;
- **beim Minimieren** einmal, bevor der Trichter beginnt: Das minimierte Fenster ist danach
  verdeckt (1 Bild/s, `HIDDEN_FRAMES`), sein Bild muss nicht leben.
  Beim Zurückholen ist das Fenster sofort wieder gezeigt (Modell): Der Trichter liest dann den
  lebenden Puffer, und seine Oberflächen zählen als gezeigt (Rückrufe im vollen Takt, FIFO und
  `hold_read` wie bei einem sichtbaren Fenster), wie KWins `OffscreenEffect`.

Die Kopie wird freigegeben, wenn ihre Animation endet.

### 3. Ausgewertet für den vblank, an dem das Bild erscheint

Vor dem Zeichnen kennt der Compositor den vblank, für den das Bild geplant ist (`Gpu::aim`:
der geplante, sonst der nächste; der Zeichen-Timer nutzt ihn schon für `release_timed`).
Animationen werden **für diesen Zeitpunkt** berechnet, wie `targetTimestamp`, in `render`
vor dem Zeichnen. Das gilt auch für die Startblende und das Abblenden im Leerlauf, die heute
`began` nehmen. Ein Bild, das ausfällt, macht die Bewegung nicht langsamer. Das nächste
zeigt die Feder dort, wo sie zu diesem Zeitpunkt sein muss. Eine laufende Animation fordert
ihr nächstes Bild wie die Startblende beim „frame done“ an. Der Takt der Clients bleibt
unberührt. Ist alles in Ruhe, fordert nichts ein Bild an. **Ohne sichtbaren Bildschirm** (aus,
gesperrt, kein Ausgang) springt jede Animation an ihr Ende. Sonst würde sie ohne Bild nie
enden und den Compositor weiter wecken. Heute betrifft das schon die Startblende.

### 4. Wer zeichnet was

- **Der Compositor** bewegt ganze Flächen: Fenster (auch fremder Apps), Popups, Menüs, das
  Kontrollzentrum, Dialoge und das Bild beim Einrasten. Wie bei macOS' WindowServer, Mutter
  und KWin zeichnet keine App diese Bewegung selbst und muss dafür auch kein Bild liefern.
  Bewegte Flächen werden als Elemente um die Oberfläche gezeichnet, mit deren ID (wie die
  Vorschaubilder von Alt+Tab), so gelten sie als gezeigt.
- **Die Shell** sagt, was nur sie weiß, über `beize_shell_v1`:
  - das Rechteck jedes App-Symbols im Dock (Ziel beim Minimieren, Ursprung beim Öffnen).
    Vorbild ist Plasmas `set_minimized_geometry` (früher `_KDE_NET_WM_ICON_GEOMETRY`);
  - dass eine ihrer Flächen ein Dialog ist, der erscheint, und dass er schütteln soll. Den
    Dialog bewegt der WindowServer auch bei macOS (SecurityAgent ist ein Fenster). Sein Glas
    gehört dem Compositor und muss mitgehen.
- **Ursprung beim Start aus dem Dock:** Das Dock holt sich ein Aktivierungs-Token
  (xdg-activation), und der Compositor nimmt Tokens der Shell an, wie GNOME seine
  Startsequenzen selbst erzeugt. Heute verwirft `activation.rs` jedes Token, das nicht von der
  App mit der Tastatur kommt. Die App startet in einer transienten Unit mit
  `XDG_ACTIVATION_TOKEN` in der Umgebung (GNOME, KDE). Heute ist es `StartUnit` einer
  Template-Unit, das keine Umgebung mitgeben kann. Erscheint das erste Fenster mit diesem
  Token, wächst es aus dem Symbol. Sonst öffnet es am Ort.
- **Beize UI** animiert, was in einer Fläche passiert (Auswahlring bei Alt+Tab, Schalter,
  Felder), mit derselben Feder im Takt seiner frame callbacks.

### 5. Katalog

| Was | Wie | Feder (d s / b) | Vorbild |
| --- | --- | --- | --- |
| Öffnen am Ort | von 94 % mit Einblenden | 0,3 / 0 | macOS 26; gnome-shell 150 ms |
| Öffnen aus dem Dock | aus dem Symbol, der Trichter rückwärts | 0,55 / 0 | VISION; macOS „Genie“ |
| Schließen | auf 94 % mit Ausblenden, eigenes Bild | 0,25 / 0 | macOS 26; gnome-shell 150 ms |
| Minimieren, zurück | Trichter ins Symbol und zurück | 0,55 / 0 | macOS „Genie“, KWin „Magic Lamp“ |
| Einrasten, Maximieren | Rahmen federt vom alten zum neuen Rechteck; das alte Bild blendet ins neue über, gestreckt auf den Rahmen | 0,4 / 0, Überblendung 0,25 / 0 | gnome-shell 250 ms mit Klon, Windows 11 |
| Menü | erscheint sofort; schließt mit Ausblenden seines Bilds | aus: 0,2 / 0 | macOS |
| Kontrollzentrum | wächst aus seinem Knopf, zurück dorthin | 0,4 / 0,15 auf, 0,25 / 0 zu | macOS 26, `.snappy` |
| Dialog | von 94 % mit Einblenden; zu schneller | 0,3 / 0 auf, 0,2 / 0 zu | `.smooth` |
| Falsches Passwort | Stoß zur Seite, erster Ausschlag 10 px | 0,15 / 0,7 | macOS-Autorisierungsdialog |
| Alt+Tab | erscheint nach 150 ms ohne Animation (`SHOW_AFTER`), der Auswahlring federt | 0,15 / 0,14 | macOS ⌘⇥, `interactiveSpring` |
| Desktop einblenden | gebaut | 0,5 / 0 | `.smooth` |
| Vollbild | springt, wie heute | – | eigene Entscheidung mit Spaces |

Nachschwingen gibt es nur beim Kontrollzentrum (0,6 %, etwa 2 px) und beim Ring (0,5 %),
gerechnet. Das ist spürbar weich, aber nicht als Federn zu sehen.

**Menü:** macOS lässt den gewählten Punkt einmal blinken, bevor das Menü verschwindet. Das
setzt Beize nicht um: Solange das Menü blinkt, hält sein Popup den Grab, und Tasten gingen
an das Menü statt an die App (Abschnitt 2). Das Popup schließt sofort, der Compositor blendet
sein eigenes Bild aus, in dem der gewählte Punkt noch hervorgehoben ist.

**Der Trichter** (Besitzer, 17.11.: „wirklich diese Verformung“): Eine Feder p von 0 bis 1
treibt zwei überlappende Abschnitte, wie die bekannten Nachbauten und Magic Lamp:
- **Biegen** bis p = 0,5: Die Seiten folgen von der Oberkante des Fensters bis zur Oberkante
  des Symbols der glatten Kurve 3n² − 2n³ (stetig in Ort und Steigung, keine Knicke).
  Darunter ist der Trichter so breit wie das Symbol, und seine Unterseite rückt mit dem
  Biegen unter das Symbol.
- **Gleiten** von p = 0,4 bis 0,9: Der Inhalt gleitet durch den Trichter hinein. Die
  Unterkante erreicht das Symbol nach 1/1,6 des Gleitens (bei p ≈ 0,71), so staucht sich der
  Inhalt im Hals, statt über das Symbol zu ragen.
- **Ende:** Bei p = 0,9 ist das Fenster ganz im Symbol. Darüber ändert sich nichts mehr, das
  Minimieren endet dort, nach etwa 0,34 s (0,62 · d, gerechnet; gnome-shell 400 ms). Ohne
  diese Grenze schöbe die Feder die letzten Pixel noch fast eine Sekunde lang ins Symbol
  (Prüfung 17.11.). Das Zurückholen läuft denselben Weg rückwärts, seine Feder endet nach §1.
  An jeder Stelle unterbrechbar.

Gezeichnet wird das eigene Bild (§2) als Gitter, und der Vertex-Shader verbiegt die Punkte,
wie bei Qts und KWins Genie. Es gibt eine Zeile etwa alle 4 physische Pixel der Höhe und nur
so viele Spalten, wie die Kurve braucht. Das Gitter ist zusammenhängend, deshalb gibt es keine
Spalten zwischen den Zeilen. Die Kanten glättet derselbe Rand wie bei den runden Ecken. Der
Schatten blendet in den ersten 15 % aus, denn er gehört zu einem Rechteck. Liegt das Symbol
seitlich (ein Dock an der Seite kommt später), läuft der Trichter zu dieser Kante.

**Das Schütteln** kommt mit dem Urteil von PAM, also nach der Wartezeit von `pam_faildelay`
(ADR 0031), wie bei macOS erst nach der Pause. Es ist ein Stoß in einer Feder, keine
Keyframe-Kurve, und die einzige Bewegung eines Dialogs, die nachschwingt. Der erste Ausschlag
ist 10 px (etwa 3 % der Dialogbreite) nach 32 ms. Danach fällt er je Halbschwingung auf
0,37 (10 / 3,7 / 1,4 / 0,5 px) und ist nach ~0,3 s unsichtbar. Das sind zwei bis drei
sichtbare Ausschläge, ruhig statt zappelig. Das Feld wird leer und behält die Tastatur, die
rote Zeile erscheint. Die Bewegung unterstreicht nur, sie ist nicht das einzige Signal.

Größenwechsel animiert nicht den Inhalt Bild für Bild mit (wie macOS' „Zoom“). Das würde
jedes Bild auf die App warten lassen, und eine langsame App würde die Bewegung hängen lassen.
Der neue Ort kommt weiter erst mit dem Commit der neuen Größe (ADR 0028 §2). Ab diesem Commit
federt der Rahmen, nicht vorher.

**Bewegung reduzieren** kommt mit den Einstellungen. Dann blendet nur über, ohne Ortswechsel
und ohne Schütteln (Apples „Reduce Motion“).

### 6. Woher die Werte kommen

Apples eigene Werte sind nicht zu haben (Kontext). Deshalb gelten diese Regeln, jede mit
Vorbild:
- **Federn aus SwiftUIs Vorgaben** (`.smooth` ohne Nachschwingen, `.snappy` 0,15,
  `interactiveSpring` 0,15 s/0,14). Keine eigenen Kurven.
- **Fenster und Dialoge schwingen nicht nach**, ausgenommen das Schütteln. Nur was aus einem
  Knopf wächst oder direkt bedient wird (Kontrollzentrum, Alt+Tab-Ring), schwingt höchstens
  0,15 nach. Liquid Glass federt so, und mehr wäre Spielerei (Besitzer, 17.11.).
- **Abgänge sind schneller als Auftritte** (Material Design und Apples HIG: Was geht, braucht
  keine Aufmerksamkeit mehr).
- **Dauer nach Weg:** gleiche Bewegung, gleiche Feder. Kleine Wege (Ring, Dialog, Öffnen am
  Ort) 0,15–0,3 s, Rahmen 0,4 s, der Trichter 0,55 s, weil er den weitesten Weg hat. Eine
  Bewegung bei b = 0 ist bei 0,62 · d zu 90 % und bei 1,06 · d zu 99 % am Ziel.
- Ein unabhängiger Agent hat die Werte gegen diese Regeln, die Entwürfe und gnome-shell/KWin
  gerechnet (17.11.). Die letzte Abnahme macht der Besitzer auf der Hardware. Ändert er einen
  Wert, gilt seiner.

### 7. Prüfen

- **Tests:** die Feder (gebaut: Apples Beispielwerte, kein Überschwingen bei b = 0,
  Überschwingen nach der Formel, Stetigkeit von Wert und Geschwindigkeit beim Umzielen, ein
  großer Schritt gleich vielen kleinen, das Ende ohne sichtbaren Rest, ein Stoß klingt ab);
  die Geometrie des Trichters (stetige Kanten, nie breiter als das Fenster, bei p = 0,9 im
  Symbol); das Ende ohne Bildschirm.
- **Smoke** prüft nur Ablauf und Ergebnis, nie den Takt (VM, ADR 0027). Damit „während der
  Animation“ belegt ist, verlängert ein Faktor nur im Test alle Federn (wie
  `BEIZE_SCREEN_FADE` und GNOMEs `slow-down-factor`). Der Compositor schreibt Anfang und Ende
  jeder Animation ins Journal, und die Prüfung verlangt die Zeile „läuft“, bevor sie handelt:
  - Eine Taste während des Öffnens kommt beim neuen Fenster an.
  - Ein Klick während des Minimierens trifft das Fenster darunter.
  - Nach dem Ende fordert keine Animation mehr Bilder an.
  - Das eigene Bild eines geschlossenen Fensters wird freigegeben.
  Jede Prüfung mit Gegenprobe.
- **Messbau auf der Hardware:** `pacing:` je Animation mit 0 späten Bildern bei 240 Hz,
  „Taste bis Bild“ während einer Animation so wie ohne (heute 2–6 ms), GPU je Bild im Budget
  von ADR 0028 §9. Ein bewegtes Fenster beschädigt jedes Bild seine alte und neue Lage, auch
  unter Glas. Der Lasttest bekommt laufende Federanimationen, wie §9 es vorsieht.

## Reihenfolge

a) Feder in `beize-draw`, Startblende darauf, Auswertung an `aim`, Ende ohne Bildschirm
   (✓ 17.11.).
b) Testfaktor und Journalzeilen mit den ersten Smoke-Prüfungen; eigene Bilder des
   Compositors; Öffnen am Ort, Schließen; Dialog mit Erscheinen und Schütteln
   (`beize_shell_v1`).
c) Trichter: Gitter im Renderer, Symbol-Rechtecke vom Dock; Tokens der Shell und Start in
   einer transienten Unit, Öffnen aus dem Symbol.
d) Einrasten, Maximieren mit Überblendung; Menü, Kontrollzentrum, Alt+Tab-Ring.
e) Messbau mit allem; der Besitzer nimmt die Werte auf der Hardware ab.

## Konsequenzen

- `beize_shell_v1` bekommt die Rechtecke der Dock-Symbole, das Erscheinen eines Dialogs und
  das Schütteln.
- `activation.rs` nimmt Tokens der Shell an; Apps starten in einer transienten Unit mit dem
  Token (`beize-systemd`).
- Der Compositor zeichnet eigene Bilder von Fenstern und Popups (mit Glas) und hält sie, bis
  ihre Animation endet. Der Trichter braucht ein Gitter-Element im Renderer.
- Kommt einmal Direct Scanout, sind animierte Fenster davon ausgenommen.
- Apples Optik ist bei Animationen erlaubt, wo sie zu Beize passt und sich sauber umsetzen
  lässt (Besitzer, 17.11., der Trichter). Das ändert die Regel „Konzepte, nicht Optik“ der
  VISION für Bewegung. App-Symbole und „macOS-Look“-Sets bleiben tabu.
- Jede Animation kostet Bilder. Der Messbau zeigt, ob das Budget hält.
- ADR 0028 §6 „Animationen“ und (g) verweisen auf dieses ADR.
