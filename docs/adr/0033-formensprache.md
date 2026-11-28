# 0033 – Formensprache: wenige Grundwerte, ein Glas

Status: Angenommen · 2026-11-28 (Claude als Design-Direktor, CLAUDE.md §2; der Besitzer nimmt auf
Hardware ab)

## Kontext

Die Shell ist in Stufen entstanden, jede Stufe mit gemessenen oder geschätzten Werten (ADR 0028,
0032). Jede Entscheidung war für sich begründet, zusammen sind es zu viele: Die Bestandsaufnahme
vom 28.11. (28 Bildschirmfotos, Gutachten `beize-design-auditor`) fand

- vier Radien für die Rolle „große schwebende Fläche“ (Dialog 26, Dock 24,06, Alt+Tab 32,
  Kontrollzentrum 25) und Kapseln neben Rundrechtecken ohne Regel;
- 17 verschiedene Abstände (5 bis 28), kein Raster;
- fünf Schriftgewichte für drei Rollen (400, 500, 540, 600, 700) und Größen wie 14,03 und
  12,16, die aus einem Faktor entstehen;
- drei Schwarz-Tinten (#1a1a1a, #1d1d1d, #292929), die Akzentfarbe doppelt definiert, elf
  Deckkräfte für „neutrale Fläche“;
- sechs Glasrezepte, dazu Rauch, Abdunklung und drei Pillenrezepte. Das flüssige Glas (Dock,
  Kacheln des Kontrollzentrums) hat keinen eigenen Körper und lebt von vier Kanteneffekten; seine
  Tonkurve ändert den Charakter mit dem Hintergrund. Gemessen: weiße Schrift im hellen
  Kontrollzentrum 3,3 : 1, Unterzeile 2,4 : 1; das Dock ist über hellem Grund kaum zu sehen.

Der Besitzer (28.11.): Beize soll einen eigenen Charakter haben, alles aus einem Guss; das
flüssige Glas ist „nichts Ganzes“, das **Menü-Glas ist „mega“**. Überwältigen soll ein System,
nicht Effekte: die Haltung der Bediensysteme von smart (#1/#3) und Li Auto (L9) – wenige
Grundwerte und Materialien, konsequent, ruhig, hochwertig –, nicht ihre Form. Beize bleibt ein
Desktop für Maus und Tastatur.

## Entscheidung

### 1. Der Charakter

**Ruhig und dicht.** Flächen sind aus einem Glas, das Licht hält statt mit ihm zu spielen: eine
gleichmäßige Eigenhelligkeit, durch die der Hintergrund nur als Farbe scheint. Form entsteht aus
dem weichen Schatten und der Rundung, nie aus Lichtkanten. Farbe ist selten und hat eine
Bedeutung (Akzent = Auswahl und Hauptaktion, Rot = Gefahr). Jede Fläche folgt denselben wenigen
Werten, sodass das Auge nach kurzer Zeit nichts mehr prüft – das ist das Hochwertige an den
Vorbildern, nicht ihre Grafik.

### 2. Ein Raster

Alle Abstände und Größen sind Vielfache von **4 px** (bei Faktor 1): 4, 8, 12, 16, 20, 24, 32,
48. 2 px nur für Haarlinien und optische Versätze. Was heute aus HIG-Maßen × 1,25 stammt, rastet
auf den nächsten Wert ein (Menüzeile 30 → 28, Apples 22 pt × 1,25 = 27,5).

### 3. Radien aus einer Regel

**Außen = innen + Abstand.** Drei Stufen, jede aus der nächsten:

| Stufe | Radius | Rolle |
| --- | --- | --- |
| Fläche | 28 | große schwebende Flächen: Dialog, Alt+Tab, Mitteilungen; eine Kachel 2 × 2 |
| Element | 16 | Fenster, Menüs, Kapseln und Kacheln in einer Fläche (Fläche 28 − Abstand 12) |
| Detail | 12 | Hervorhebung einer Menüzeile, Eingabefeld, Vorschaubilder (Element 16 − Abstand 4) |

Knöpfe sind Kapseln mit **32 px** Höhe (Radius 16 = Element). In einer Fläche liegen sie 12 vom
Rand, damit sind sie konzentrisch. Freier Text beginnt bei **20**: Das ist der Rand der Kapsel
(12) plus ihr Innenabstand (8), so stehen Text und Knopfbeschriftung auf einer Linie. Das Dock
behält seine auf Hardware abgenommenen Maße (Besitzer, 25.10.): Symbole 56, ringsum 17, 90 hoch,
Radius 24 (`DOCK_INSET`, `DOCK_RADIUS`); das Raster von 84/28 sah er am 28.11. als Rückschritt. Die
stetigen Ecken (ADR 0028 §4) bleiben für alles außer Kapseln und Kreisen.

### 4. Schrift

Inter, wie bisher; Terminal JetBrains Mono.

- **Zwei Größen:** 15 (Text, Menüs, Leiste, Titel) und 13 (Nebentext, Beschriftungen).
- **Drei Gewichte:** 400 Text, 500 Bedienelemente (Leiste, Knöpfe, Menütitel), 600 Titel
  (Fenstertitel, Dialogtitel, Name der aktiven App, Titel im Kontrollzentrum). 540 und 700
  entfallen.
- Keine Größe aus einem Faktor: das Kontrollzentrum nutzt dieselben 15 und 13.

### 5. Farben nach Bedeutung

Jede Farbe ist ein Token mit einer Fassung für Hell und für Dunkel. Gewählt wird nach der Rolle,
nie nach dem Wert.

| Token | Hell | Dunkel | Rolle |
| --- | --- | --- | --- |
| Tinte | #1d1d1d | #f2f2f2 | Text, Symbole |
| Tinte 2 | #5e5e5e | #a3a3a3 | Nebentext, Kürzel, Unterzeilen |
| Tinte 3 | Tinte 30 % | Tinte 35 % | gesperrt |
| Fläche | Schwarz 7 % | Weiß 8 % | neutrale Kachel, Knopf, Feld auf Glas |
| Fläche gewählt | Schwarz 12 % | Weiß 16 % | offener Knopf der Leiste, gewählte Kachel |
| Akzent | #0064d2 | #0064d2 | Auswahl, Hauptaktion; Weiß darauf 5,6 : 1 |
| Gefahr | #c4001a | #ffb3ae | Fehlertext, zerstörende Aktion; ≥ 4,5 : 1 auf dichtem Glas |

Tinte 2 ist eine eigene Farbe, keine verblasste Tinte: In linearem Licht gemischt, wie der
Compositor mischt, hält Tinte zu 65 % über hellem Glas über Schwarz nur 2,5 : 1; ebenso
setzt der Test (§9) das dunkle Fehlerrot und die dunkle Fläche fest (gerechnet 28.11.:
#ff6961 hielt 2,8 : 1, Weiß 9 % unter Tinte 4,4 : 1).
| Linie | Schwarz 8 % | Weiß 10 % | Trennlinie in Menüs und Flächen |

Der Akzent ist Beizes eigener, ein tieferes, ruhigeres Blau als Apples #007aff (Weiß darauf
4,0 : 1), in beiden Erscheinungsbildern derselbe: ein helleres Blau für Dunkel trüge weiße Schrift
nur mit 3,3 : 1 (#3d8bff, gerechnet). Mit den Einstellungen wird er wählbar (VISION). Die Schrift auf Glas folgt dem Erscheinungsbild: dunkle
Tinte im hellen, helle im dunklen Modus – auch im Kontrollzentrum (heute dort immer Weiß). Die
Menüleiste ohne Hintergrund behält ihre Regel aus ADR 0028 §3 (Tinte nach dem gemessenen
Hintergrund), weil sie auf dem Bild liegt, nicht auf Glas.

### 6. Ein Material: Beize-Glas

Ein Rezept, das des Menü-Glases von heute: Dual Kawase über das, was dahinter liegt, Sättigung
× 1,8, eine **neutrale Tönung**, die der Fläche ihre konstante Eigenhelligkeit gibt
(#F2F2F2 bzw. #2D2D2D), eine gleichmäßige schwache Lichtkante innen (0,3 / 0,12), außen auf dem
äußersten Pixel eine dunkle Haarlinie (Schwarz 16 % / 30 %; Apples Dock und Menüs haben sie, sie
hebt helles Glas von hellem Grund ab), feines Korn. Es gibt **drei Dichten**, sonst nichts:

| Dichte | Tönung | Wo |
| --- | --- | --- |
| dicht | hell 80 %, dunkel 94 % (der Test unten legt es fest) | alles, was Text trägt: Menüs, Kontextmenüs, Dialoge, Alt+Tab, die Kacheln des Kontrollzentrums, Mitteilungen, die Menüleiste über Vollbild |
| mittel | hell 55 %, dunkel 70 % | Dock (ohne Text; dichter wirkte es milchig, dünner nahm es die Farbe des Hintergrunds zu stark an, Gutachten 28.11.) |
| dünn | 30 % | Menüleiste mit Hintergrund |

Das **Kontrollzentrum** behält sein Layout (ADR 0028 §5): einzeln schwebende Kacheln, eine Kapsel
und ein Kreis, ohne Fläche darum, jetzt aus dichtem Glas. Eine Fläche mit Kacheln darauf war am
28.11. gebaut und gefiel dem Besitzer nicht (aufdringlich, zu groß). Seine Pille in der Leiste
und die des Beize-Menüs werden mit Stufe 2 gleich: „Fläche gewählt“.

Die **Menüleiste über einem Vollbild** liegt auf dichtem Glas, die Schrift nach dem
Erscheinungsbild: so hält sie über jedem Vollbild den Kontrast des Tests (auf dünnem Glas über
einem weißen Vollbild hielt helle Schrift ~1,3 : 1). macOS zeigt die Leiste dort deckend.

**Entfallen:** das flüssige Glas (`clear`, `liquid`: Wölbung, Lichtbrechung, Lichtlinien, Schein,
Seitenabdunklung, Milch, Tonkurve, Farbvertiefung), der Rauch, der Schatten `LIQUID`. Das System
wird einfacher und billiger: ein Blur je Fläche statt Gauß mit Tönung je Quad und Rauchmaske.

Vorbilder: NSVisualEffectView (ein Mechanismus, Materialien als Dichten: `.menu`, `.popover`,
`.hudWindow`), Windows 11 (Acrylic, Mica, Mica Alt: eine Familie), KDE (ein Blur, eine
Deckkraft).

### 7. Drei Höhen

| Höhe | Schatten (Versatz / Sigma / Deckkraft) | Wo |
| --- | --- | --- |
| Fenster | aktiv 10 / 14 / 0,35, inaktiv 4 / 8 / 0,20 | Fenster |
| schwebend | 6 / 12 / 0,25 (Apples Menüs) | was über Fenstern liegt und bei einem Klick daneben geht: Menüs, Dialoge, Alt+Tab, Mitteilungen |
| aufliegend | 3 / 1,5 / 0,12, seitlich 2 eingerückt (Besitzer 30.10. auf Hardware) | was Teil des Schreibtischs ist: Dock, Kacheln des Kontrollzentrums |

Die App bestellt die Höhe mit ihrem Glas (`beize_glass_v1.set_shadow`: `floating`, `resting`).
Ein Schatten 8 / 24 / 0,18 für alles war am 28.11. gebaut: unter dem Dock ein breiter Hof, der
64 px auf Fenster fiel.

### 8. Bewegung

ADR 0032 bleibt, wie abgenommen. Die Federn werden Tokens (Dauer und Federung), die Werte ändern
sich nicht.

### 9. Wo die Werte wohnen

**Ein Modul `beize-draw::tokens`** (die unterste Schicht, die Beize UI, die Shell und der
Compositor teilen) hält Raster, Radien, Schrift, Farben, Höhen und Federn. Kein Zahlwert für eine
dieser Rollen steht mehr in einer anderen Datei; was in den Bausteinen bleibt, sind Ableitungen
(„Kapsel = Element“, „Text = Rand + Innenabstand“). Das Material zeichnet weiter der Compositor;
Apps bestellen es über `beize_glass_v1` mit dem Namen der Dichte (`dense`, `medium`, `thin`)
statt mit heute sechs Materialien.

Ein Unit-Test in `tokens` rechnet jedes Paar aus Tinte und Untergrund in linearem Licht, wie der
Compositor mischt, und zwar über dem schwersten Fall jedes Erscheinungsbilds: helles Glas über
Schwarz (ein Terminal dahinter), dunkles Glas über Weiß (ein helles Fenster dahinter). Tinte,
Gefahr und Weiß auf Akzent müssen dort WCAG AA halten (4,5 : 1), auf „Fläche“ ebenso, auf
„Fläche gewählt“ 3 : 1 (Beschriftung von Bedienelementen, WCAG 1.4.11; in jedem anderen Fall weit
mehr), Tinte 2 dort 3 : 1 und über Glas über dem eigenen Hintergrundbild AA; gesperrte Einträge
sind ausgenommen. Ein neuer Wert, der das bricht,
kommt nicht durch `bz check`. Gerechnet am 28.11.: Das dunkle Menü-Glas von heute (80 %) hält
über einem weißen Fenster weiße Schrift nur mit etwa 3,5 : 1 – das Dunkle braucht mehr Dichte als
das Helle, wie Apples dunkle Menüs deckender sind; mit „Fläche“ darauf hält erst 94 %.
Dünnes und mittleres Glas tragen keinen Text, außer der Menüleiste über Vollbild: Dort hält
helle Schrift auf dunklem dünnem Glas über einem weißen Vollbild nur ~1,3 : 1 (vor ADR 0033
ebenso, Grundglas 45 %); das gehört zu Stufe 3 (Menüleiste).

## Umsetzung

In Stufen (STATUS, Plan B3). Jede Stufe zieht ihren Bereich auf die Tokens um, ist danach
pixelgenau in der Galerie (`bz smoke --only gallery`) zu sehen, wird vom Gutachter geprüft, und
der Besitzer nimmt sie auf Hardware ab:

1. **Material:** `tokens`, Beize-Glas in drei Dichten, Dock und Kontrollzentrum darauf; flüssiges
   Glas und Rauch werden entfernt.
2. **Menüs, Dialoge, Feld, Alt+Tab, Pillen:** Radien, Abstände, Farben; Gefahr mit 4,5 : 1;
   die Auswahl in Alt+Tab als „Fläche gewählt“ statt Vollblau.
3. **Fenster, Titelleiste, Menüleiste.**
4. **Schrift:** parley 0.13 (Dependabot #45) mit den zwei Größen und drei Gewichten.

## Konsequenzen

- Sichtbar ändern sich: das Material von Dock und Kontrollzentrum (Beize-Glas statt flüssigem,
  dunkle Schrift im Hellen), die Menüleiste über Vollbild, Radien und Abstände von Dialog, Alt+Tab
  und Feld, der Akzent, das Fehlerrot, Gewichte. Layout und Maße von Dock und Kontrollzentrum
  bleiben, wie abgenommen.
- Weniger Code: `clear_glass`, `liquid_tone`, die Rauchmaske und ihre Smoke-Prüfungen gehen;
  neue Prüfungen kommen für die Dichten und die Kontraste.
- Neue Flächen (Mitteilungen, Suche, Sperrbildschirm, Einstellungen) haben ihre Werte schon,
  bevor sie gebaut werden.
- ADR 0028 §5 und §6 (Kontrollzentrum, Material) werden mit Stufe 1 auf diese Fassung
  gebracht, nicht ergänzt.
