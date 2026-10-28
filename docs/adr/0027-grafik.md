# 0027 – Grafik: Compositor, Vulkan, Toolkit, NVIDIA, Schrift

Status: Angenommen · 2026-10-17 (mit dem Besitzer besprochen; NVIDIA nach der Messung in
Schritt 1: NVK) · Abschnitt 6 (Gerüst) angenommen 2026-10-21

## Kontext

M3 bringt die Oberfläche. `docs/VISION.md` lässt offen: Grafik-API des Compositors, UI-Toolkit,
Systemschrift, Icon-Stil, NVIDIA-Treiber (#17). Maßstab ist macOS: alles Sichtbare aus einem
Guss, vom System selbst gebaut. Feste Regeln von Beize, die hier hineinspielen: immer der
neueste stabile Kernel (zurzeit 7.2.10), keine fremden Patches, alles aus dem Quellcode und
signiert, nur der Desktop bekommt Grafik (ADR 0025). Zielhardware: RTX 3070 Ti; der Monitor
des Besitzers kann 1440p @ 240 Hz. Unter nouveau bot KMS bei der Messung für 1440p nur bis
143,9 Hz an (Ursache offen, siehe Abschnitt 4). Maßstab bleibt das Budget der VISION: 4,2 ms
je Frame (240 Hz), Shell < 1 ms GPU-Zeit.

Stand der Bausteine (Oktober 2026):

- **Smithay** (0.7.0, neueste Version auf crates.io) bringt Wayland-Protokoll, DRM/KMS, GBM,
  libinput, Puffer und Damage-Tracking. Eingebaut sind nur GLES-, glow- und pixman-Renderer, ein
  Vulkan-Renderer existiert nur in Forks und fremden Crates (COSMIC plant einen, Otto und ein
  niri-Fork haben experimentelle). Den fertigen Ausgabe-Baustein `DrmCompositor` gibt es nur mit
  dem Feature `backend_gbm`. Ohne GBM bleiben `DrmSurface` (atomare Commits), `Swapchain`,
  `VulkanAllocator` (Vulkan-Bilder als dmabuf) und `OutputDamageTracker`.
- **NVIDIA-Treiber** hinkt neuen Kernels hinterher: 590-open baute auf 6.19 erst mit Patch,
  NVIDIA lieferte 580.126.18 nach; 7.0 lief nur mit Community-Patches. Die Bibliotheken
  (Vulkan, GBM) sind geschlossen.
- **Nouveau + NVK** (Mesa) folgen dem Kernel von selbst, laufen auf Ampere über die
  GSP-Firmware aus linux-firmware, sind in Spielen deutlich langsamer als NVIDIA. Messwerte
  für Ampere/3070 Ti fehlen öffentlich.
- **Toolkits:** Xilem/Masonry sind ausdrücklich Alpha mit großen Umbrüchen; Slint und iced
  hat der Besitzer als unscharf bzw. zu eingeschränkt verworfen.

## Entscheidung

### 1. Eigener Compositor auf Smithay, Shell daneben

Ein eigener Compositor (`os/beize-compositor`): Fenster, Eingabe, Effekte (Blur, Schatten,
Ecken, Animationen) und Bildschleife. Smithay übernimmt Protokoll, KMS und Eingabe und bleibt
unverändert (Crate aus crates.io, keine Forks). Einzige Ausnahme, befristet: ein Fehler, den
Beize upstream meldet und dessen Patch dort liegt, kommt bis zum nächsten Smithay-Release per
`[patch.crates-io]` von einem festen Commit (Fork des Besitzers: das Release + nur dieser
Patch), mit Verweis auf den Upstream-PR; mit dem Release fällt er weg. Heute: importierte
syncobj-Timelines zerstört Smithay 0.7.0 nie (je Swapchain-Bild zwei, also bei jeder
Größenänderung einer Mesa-App). Menüleiste, Dock und Kontrollzentrum zeichnet
ein eigener Prozess mit Beize UI, aus derselben Hand (ADR 0028). Vorbild: macOS' WindowServer
mit Dock und Menüleiste als eigenen Prozessen.

### 2. Nur Vulkan, eigener Renderer

Kein GLES-Zweig. Der Renderer (`ash`) implementiert Smithays `Renderer`-Schnittstelle: dmabuf-Import,
explizite Synchronisation (`linux-drm-syncobj-v1`), Damage, Direct Scanout, die Effekte der
VISION (Kawase-Blur auf ¼–⅛ Auflösung, gecachte Schatten). Ein Code-Pfad auf jeder
Maschine: auf Hardware der Treiber der GPU, in VM und CI **lavapipe** (Mesa, Vulkan auf der
CPU). Vorbild: macOS kennt nur Metal. Smithays GLES bleibt ungenutzt.

### 3. Toolkit „Beize UI“ – eigen, auf bewährten Bausteinen

Eigen sind Widgets, Stil, Federanimationen und der Renderer (gemeinsam mit dem Compositor).
Fremd, weil dort Jahre Fachwissen stecken: **Parley** (Textlayout, Shaping über HarfRust,
Fontique), **Skrifa** (Glyphen mit Hinting), **Taffy** (Layout), **AccessKit**
(Barrierefreiheit). Eingabemethoden über `text-input-v3`. Vorbild: AppKit/SwiftUI.

**Schärfe** ist Teil der Architektur, nicht des Feinschliffs: Fenster rendern direkt in
physischen Pixeln (`wp_fractional_scale_v1` + `wp_viewporter`), der Compositor skaliert nie
herunter; Glyphen am Pixelraster gerastert, Kanten und Linien eingerastet. Unschärfe bei
anderen Toolkits kommt fast immer vom Rendern mit ganzzahligem Faktor und Herunterskalieren.

### 4. NVIDIA: NVK, wie Fedora

Der Besitzer will den stabileren und verbreiteteren Weg und orientiert sich an Fedora
Workstation. Fedora hat dieselbe Lage wie Beize (neuester stabiler Kernel): ab Werk Nouveau +
NVK, der NVIDIA-Treiber kommt von außen (RPM Fusion) und muss bei neuen Kernels nachziehen.
Ubuntu liefert NVIDIA, aber mit älteren Kernels.

**Regel:** NVK wird Vorgabe, wenn es das Budget mit Compositor-Last hält. NVIDIA nur, wenn
NVK das nicht schafft **und** der Treiber auf dem aktuellen Kernel ohne Patch baut.

**Messung (2026-10-17, Beize 0.5.92, Tag `measure/nvk-0.5.92`):** `beize-gpu-probe`, direkt
auf KMS über `VK_KHR_display`, FIFO, 30 s Compositor-Last (12 Fenster mit runden Ecken und
Schatten, Live-Blur-Panel über ½ und ¼ Auflösung), NVK (Mesa 26.2.5) mit GSP 570.144:

| | Ergebnis |
|---|---|
| Modus | 2560×1440 @ 143,912 Hz (Budget 6,949 ms) |
| Bilder | 4306, 143,52/s |
| Bildabstand | Median 6,949 ms, p99 7,013 ms, max 107 ms |
| Verpasst | 1 (sehr wahrscheinlich der erste Modeset) |
| GPU-Zeit der Last | Median 2,51 ms, p99 2,62 ms, max 3,0 ms |
| Temperatur, Leistung | nouveau meldet für die Karte keine (kein hwmon) |

**Ergebnis: NVK.** Die Last braucht 36 % des Budgets bei 144 Hz und hielte auch 240 Hz
(4,2 ms). **240 Hz:** nouveau meldete für 1440p keinen 240-Hz-Modus, obwohl der Monitor (Dell
AW2723DF, DisplayPort) ihn kann und unter Windows damit läuft. Der Modus steht nur im
DisplayID-Block des EDID (1056,75 MHz) und passt mit 8 Bit je Farbe ohne DSC in HBR3 × 4 (25,4
von 25,9 Gbit/s); der Link lief mit HBR3 × 4. Ursache ist ein Fehler in nouveau mit GSP: Der
EDID-Block 2 kam als Kopie von Block 0 an, weil der I²C-Start zum STOP wurde und der
Segment-Zeiger verloren ging. Der Fix ist auf der Hardware des Besitzers getestet (EDID
byte-gleich mit Windows, 240 Hz in der Modusliste) und geht upstream. Beize übernimmt ihn mit
dem Kernel, nicht als eigenen Patch.

Den NVIDIA-Testbau gibt es nicht: Er hätte `bz` Fremdmodule signieren lassen, und
NVIDIA 615.78.08 baut zwar auf 7.2.10, objtool meldet aber rund 20 Rücksprünge in
`nvidia.ko` ohne Retbleed-Schutz. Leistungsaufnahme im Leerlauf misst später ein
Strommessgerät, da nouveau sie nicht meldet.

**Atomic KMS:** nouveau bietet die atomare KMS-Schnittstelle nur mit `nouveau.atomic=1`
(Vorgabe aus). Ohne sie fehlt dem Connector `CRTC_ID`, Mesas `VK_KHR_display` findet keinen
Bildschirm (erste Messversuche: `VK_ERROR_UNKNOWN`). Moderne Compositoren (Smithay, KWin,
Mutter) arbeiten bevorzugt atomar, Overlay-Ebenen und VRR setzen es voraus. Beize setzt den
Upstream-Schalter in `/usr/lib/modprobe.d/beize-nouveau.conf`; die Messung lief damit.

### 5. Schrift und Icons

Systemschrift **Inter** (OFL, variabel mit optischen Größen, nah an SF Pro), Terminal
**JetBrains Mono** (OFL). Icons: eigener Satz, weich und abgerundet. Entschieden wird er mit
der Shell (Schritt 4), Apples Icons und Symbole sind tabu.

### 6. Gerüst: Sitzung, Start, Bildausgabe, Test

- **Sitzung:** Der Compositor läuft als angemeldeter Nutzer, nie als root. DRM- und
  Eingabegeräte öffnet systemd-logind für ihn (`TakeDevice`), angesprochen über **libseat**
  mit nur dem logind-Backend (aus seatd, ohne dessen Dienst). Keine Gruppen wie `video` oder
  `input`, kein setuid. Vorbild: GNOME und KDE auf Fedora.
- **Start:** Bis Beize ein eigenes Anmeldefenster hat (wie macOS' loginwindow, eigenes ADR),
  meldet man sich auf tty1 an und startet die Oberfläche mit `beize-session`. Das startet die
  Benutzer-Unit `beize-compositor.service` in `graphical-session.target`, wie GNOME seine
  Shell als Benutzer-Unit führt. Kein Autostart über Shell-Profile. Umgesetzt (22.10.) wie
  gnome-session und startplasma: `beize-session` bleibt in der Anmeldesitzung, gibt dem
  Benutzermanager `XDG_SESSION_ID` (libseat findet damit die Sitzung, obwohl der Compositor
  nicht in ihr läuft), `XDG_SESSION_TYPE=wayland` (libseat meldet es logind) und
  `WAYLAND_DISPLAY=wayland-0` (fester Name wie bei mutter und KWin, ein Compositor je Nutzer)
  und startet `beize-session.target` (bindet `graphical-session.target` und den Compositor).
  Es wartet auf das Ende des Start-Auftrags (`JobRemoved`, wie `systemctl`), dann auf das Ende
  des Compositors (nicht des Targets: das ist nach ihm geordnet und stoppt vor ihm; 22.10. im
  Smoke als Rennen aufgefallen), und nimmt die Umgebung zurück. Endet der Compositor, kehrt es zur Shell zurück
  und meldet einen Absturz; endet die Anmeldung (SIGHUP/SIGTERM), stoppt es den Desktop.
  Kein Neustart nach einem Absturz: die Sitzung endet, wie macOS beim Tod des WindowServers.
  Gesprochen wird über den privaten Socket des Managers per sd-bus (wie `systemctl --user`),
  ohne neue Bibliothek. Der Compositor meldet „bereit“ (`Type=notify`), sobald Clients sich
  verbinden können. Sandbox als Benutzer-Unit (User-Namespace, Systemaufruf-Filter),
  `systemd-analyze security` 1,2; offen bleiben müssen Geräte (Mesa öffnet den Render-Knoten
  selbst), Netlink (udev), W+X-Speicher (lavapipes JIT).
- **Bildausgabe ohne GBM:** Atomares KMS über Smithays `DrmSurface`. Die Bilder des
  Bildschirms kommen aus Vulkan (`VulkanAllocator`, `VK_EXT_image_drm_format_modifier`, mit
  den Modifiern, die KMS für die Ebene meldet) und gehen als dmabuf an KMS. Den Schritt
  dmabuf → KMS-Framebuffer (`ExportFramebuffer`) und die Bildschleife (Ebenen, Cursor,
  Page-Flip) schreibt Beize selbst, statt Smithays `DrmCompositor` zu nutzen. Begründung: GBM
  käme in Mesa nur mit einem Gallium- bzw. OpenGL-Treiber für nouveau, also genau dem zweiten
  Grafikstapel, den Abschnitt 2 ausschließt. Vorbild: Valves gamescope (Steam Deck)
  allokiert seine Ausgabebilder ebenfalls in Vulkan. Die Bildschleife ist ohnehin Beizes Kern
  (Direct Scanout, Pacing).
- **Modus:** die Auflösung des bevorzugten Modus (die des Panels) mit der höchsten Bildrate,
  die der Monitor dafür anbietet, ohne Zeilensprung; wie KWin. Der bevorzugte Modus ist bei
  schnellen Monitoren oft 60 Hz, das AW2723DF nennt 240 Hz nur im DisplayID-Block. Die
  Konsole (fbdev) nimmt den bevorzugten Modus (belegt: 60 Hz, `it had …` im Log); der
  Compositor stellt beim Start um, und der Monitor ist schwarz, bis er einrastet (~2 s).
  Ebenso beim Start, wo nouveau den Firmware-Framebuffer vor dem GSP-Start entfernt (1,74 s).
  Nur eine Umstellung, direkt auf den Modus des Desktops: wie Fedoras flicker-free boot
  (Konsole übernimmt erst bei Ausgabe, `FRAMEBUFFER_CONSOLE_DEFERRED_TAKEOVER`) mit der
  grafischen Anmeldung.
- **Ausgabe mit lavapipe** (VM, Rechner ohne Vulkan-Treiber): lavapipe exportiert dmabufs nur
  über `/dev/udmabuf` und bietet ohne Export auch keine Modifier an, kann aber importieren. Der
  Compositor legt die Bilder deshalb als Dumb-Buffer beim KMS-Treiber an. Meldet der Treiber
  `DRM_CAP_DUMB_PREFER_SHADOW` = 0 (virtio-gpu) und gleicht lavapipes Zeilenabstand dem des
  Puffers (Breite durch 16 teilbar), zeichnet lavapipe über den dmabuf direkt hinein. Sonst
  (i915, nouveau melden 1: langsam lesbarer Speicher; oder anderer Zeilenabstand) zeichnet er
  in ein eigenes Bild und kopiert. Vorbild: Mutter richtet seinen Schattenpuffer nach demselben
  Wert. Probe vom 21.10. in der VM, 1280x800, beide Bilder per `screendump` geprüft: direkt
  1,5 ms je Bild, mit Kopie 1,1 ms + 2,4 ms.
- **VM:** Die Test-VMs haben virtio-gpu (`-vga virtio`, Kernel `DRM_VIRTIO_GPU=m` wie Fedora)
  statt QEMUs Standard-VGA: atomares KMS mit Cursor-Ebene. Der Direktweg Vulkan → dmabuf → KMS
  ist dort nicht prüfbar, denn QEMU braucht für Blob-Ressourcen `udmabuf` auf dem Host, und
  WSLs Kernel hat es nicht (lokal = CI, ADR 0021). Ihn prüft der Hardware-Test mit NVK.
- **Schwache Rechner:** Der Compositor zeichnet nur, was sich geändert hat (Damage), legt
  unveränderte Vollbildfenster per Direct Scanout ohne Kopie auf den Bildschirm und zeichnet
  im Leerlauf gar nicht. Auf lavapipe (GPU ohne Vulkan-Treiber) schaltet er teure Effekte
  selbst ab: Unschärfe wird zur deckenden Fläche, Schatten bleiben gecacht. macOS bietet das
  als „Transparenz reduzieren“, Beize schaltet es dort von selbst ein. Mit Vulkan-Treiber
  bleibt der Budget-Maßstab aus Abschnitt 2 (Shell < 1 ms GPU-Zeit).
- **Test:** Ein kleiner Testclient (`beize-testclient`, reines Rust-Wayland ohne libwayland,
  `wl_shm`, festes Farbmuster) kommt über die Test-Disk in die VM, **nicht ins Image**. Der
  Smoke-Test meldet sich auf tty1 an, startet `beize-session` und den Client und vergleicht
  QEMUs `screendump` (das ausgegebene Bild) mit einer Referenz. Das Szenario läuft in der
  vollen CI-Stufe. Umgesetzt (22.10., `smoke/desktop.rs`): Anmeldung mit echten Tasten
  (deutsche Belegung), dann geprüft: Wayland-Sitzung auf tty1, Compositor als Besitzer mit
  Systemaufruf-Filter, Fenster so groß wie der Bildschirm, Bild = Muster (die Referenz
  rechnet `bz` aus, ±2 je Kanal, Abweichungen nur im 24×24-Quadrat oben links, wo der
  Zeiger beginnt; der Test bewegt ihn nie), keine Warnung außer
  Smithays bekannter, Abmelden stoppt alles. Bei Abweichung ein Differenzbild in den Logs.
- **Bildtakt messen** (22.10., `pacing.rs`): Der Compositor misst sich selbst, nicht der
  Client. Aufgabe: eine Änderung beim ersten vblank zeigen, dessen Frist sie erreicht hat
  (Zeitplan). Je Bild: Eingang der Änderung, Ziel-vblank, Beginn des Zeichnens, Commit,
  vblank, dazu wann die GPU fertig war (Zeitstempel des Kernels am Fence,
  `SYNC_IOC_FILE_INFO`). Ein späteres Bild zählt als „late“, gemessen am **Beginn** des
  vblank (dort übernimmt die Anzeige ein Bild; KMS stempelt sein Ende): Schuld beim Zeichnen
  (Commit danach), bei der GPU (Fence danach) oder bei der Anzeige (beides davor, trotzdem
  verpasst: Kernel und Treiber). Bilder in Folge ergeben eine Zeile `pacing: …` im Journal
  (Ende der Folge oder alle 30 s): Zahl, späte je Ursache, Änderung bis Bild, „ready“ (Commit
  und GPU fertig) vor dem Beginn des vblank, CPU- und GPU-Zeit je Bild (Median, 99 %, Max)
  und Auslastung des Bildbudgets, dazu der Vorlauf („lead“). Die ersten 10 späten Bilder
  einer Folge bekommen je eine Zeile `late frame (…)` mit ihren Zeiten, damit der
  Hardware-Test die Ursache je Bild zeigt. Im Leerlauf läuft kein Zeitgeber. In der VM sagen
  die Zeiten wenig, virtio-gpu hat keinen vblank.
- **Zeitplan** (`schedule.rs`): Gezeichnet wird so spät vor dem vblank, wie es sicher
  reicht, nicht am vblank; alle Änderungen bis zur Frist (Client, Zeiger) kommen in ein Bild.
  **Modell von KWin** (`RenderLoop`, `DrmCommitThread`), jeder Teil gemessen oder aus dem
  Modus: Vorlauf = erwartete Zeit vom *geplanten* Beginn bis „ready“ (Commit und Fence der
  GPU) wie KWins `RenderJournal`: geglättet über 0,5 s plus doppelte Abweichung nach oben,
  die sofort steigt und über 6 s abklingt (bis 0.5.112 das Längste der letzten 120 Bilder:
  bei 240 Hz eine halbe Sekunde, danach war ein Stau vergessen), jedes Bild zählt mit
  mindestens 2 ms (darunter sind Zeiten bei wechselnden Energiezuständen unberechenbar; wie
  KWin auf den Messwert, nicht auf die Erwartung – bis 0.5.113 auf die Erwartung, dann hob
  jedes Bild über ~1 ms die Abweichung und der Vorlauf blieb sekundenlang ein ganzes Bild)
  + Dauer des vblank (Leerzeilen × Zeilenzeit, wie Mutter;
  0,46 ms beim AW2723DF) + 1 ms für den Kernel + Gelerntes (Fehlbetrag später Bilder, je
  Bild ein Zehntel des Spielraums zurück, höchstens 3 ms oder ein halbes Bild). Gelernt wird
  aus dem Commit gegen die Frist, wie KWins Commit-Thread aus dem Ende seines Commits; die
  Zeit der GPU trägt die Erwartung (bis 0.5.113 aus „ready“: ein GPU-Stau zählte doppelt), und
  nur aus Bildern, die für einen vblank geplant waren, gegen diesen. Nach 100 Bildern Pause,
  ohne bekannte Lage der vblanks (erstes Bild, nach einem VT-Wechsel) oder nach der Frist
  zeichnet er sofort. Die Lage kommt aus den Flip-Ereignissen; ist sie älter als 1 s, fragt
  er KMS (`drmWaitVBlank` relativ 0). Der gemeldete vblank (Ereignis am Beginn, Stempel am
  Ende) ist nie der nächste. Ein Bild zeigt frühestens einen vblank nach dem, den das
  zuletzt abgeschickte frühestens erreichen konnte (KWin: ein Ziel nie vor dem zuletzt
  geplanten plus eins; nicht das vorsichtige Ziel der Messung, sonst verlöre nach einer Pause
  das zweite Bild einen vblank): nouveau wiederholt im Flip-Ereignis ab und zu den vblank
  davor (es liest die Zeilenposition, sobald die Anzeige das Bild übernommen hat, manchmal
  einen Moment zu früh; 0.5.114: 10–29 in 30 s bei 240 Hz). Als letzter genommen, schien
  der eben begonnene vblank der nächste; das Bild wurde sofort, für keinen vblank gezeichnet
  und spät gezählt, obwohl es rechtzeitig kam. Die Zeit an die Clients bleibt die des
  Kernels, wie bei KWin, Mutter und Weston: der Fehler gehört in nouveau. Einen Cursor-Zug
  allein meldet nouveau sofort (kein neues Bild, nichts abzuwarten); er zählt nicht als
  wiederholter vblank. Jede Abweichung von KWins Modell fiel auf der 3070 Ti einmal durch
  (0.5.97–0.5.100, Einzelheiten in Git); mit dem Modell 4 von 4797 Bildern spät.
  Braucht ein Bild das ganze Bild (Vorlauf = Bildperiode), beginnt es am Flip-Ereignis des
  vblank davor (dessen Beginn), nicht an dessen Stempel (Ende): 0,43 ms mehr bei 240 Hz.
  Bleibt ein Flip-Ereignis 1 s aus, sagt der Compositor es (Treiberfehler; lag es nur ungelesen,
  hing er selbst) und wartet weiter, wie KWins Commit-Thread (`s_pageflipTimeout`).
  Offen: Bei 240 Hz bleiben dem Vorlauf über KWins Mindestwerte (3,46 ms) nur 0,71 ms. Auf
  NVK beginnt die GPU etwa alle 100 ms ~2 ms nach dem Commit (0.5.112: GPU-Spanne auf
  CLOCK_MONOTONIC, „stalled“), auch ohne GPU-Clients; dazu wartet jedes Bild auf spätere Arbeit
  der Clients, weil nouveau jede Submission als Schreiber aller geteilten Puffer einträgt
  (implizite Synchronisation; „waited for … client buffers“). Abhilfe ist die explizite
  Synchronisation (§7); ob danach 0 Puffer warten, misst der nächste Hardware-Test.
  0.5.114: Die Abweichung aus diesem Stau (~1 ms) hält den Vorlauf nach KWins Formel bei
  einem ganzen Bild (geglättet 2 ms + 2 × Abweichung 1 ms + vblank 0,46 ms + Marge 1 ms
  > 4,17 ms); Änderung bis Bild 7,5 ms im Median.
  Ziel: 0 späte Bilder, Änderung bis Bild höchstens ein Bild.

### 7. Der Renderer (Schritt 2, angenommen 21.10.)

Vorher am Quelltext von Mesa 26.2.5 geprüft: NVK bietet Vulkan 1.4 ab Turing (1.3 auf
Maxwell und Pascal), lavapipe 1.4. Beide haben alles, worauf der Renderer baut.

- **Vulkan 1.3 als Untergrenze**, mit timeline semaphores, buffer device address,
  descriptor indexing, scalar block layout, dynamic rendering, synchronization2 und den
  Erweiterungen für externe Speicher, Fences und Semaphoren als dmabuf bzw. sync_file.
  Fehlt eines, nennt der Compositor es beim Namen.
- **Shader in GLSL**, beim Bau von glslang (Referenz-Compiler von Khronos) übersetzt und von
  `spirv-val` geprüft. Beides liegt schon für Mesa im Buildroot. Slang wurde geprüft: Es gibt
  es in Ubuntu 26.04, aber nicht im Buildroot (Lock-Änderung). Sein Vorteil (Module,
  Generics) zählt erst bei vielen Shadern. Die Schnittstelle ist SPIR-V, ein späterer Wechsel
  geht je Shader. Mesa legt übersetzte Pipelines in seinem Platten-Cache ab.
- **Wenige Draws:** Alle Texturen liegen in einem Descriptor-Array (bindless). Jedes
  Rechteck, das Smithay zeichnen lässt, schneidet die CPU auf seine Damage-Rechtecke zu und
  bildet es in den Framebuffer ab. Das ergibt Quads mit linearer Texturabbildung, ohne Scissor
  und in allen 8 Transformationen. Alle Quads eines Bildes stehen in einem Puffer (Adresse per
  Push-Constant), ein Draw je Lauf gleicher Art. Deckende Teile (Textur ohne Alpha oder vom
  Client als deckend gemeldet) gehen ohne Blending, das auf lavapipe CPU-Zeit kostet.
- **Schärfe:** Liegt eine Textur 1:1 auf ganzen Pixeln, liest der Shader den Texel direkt
  (`texelFetch`) statt zu filtern.
- **Die CPU wartet nicht auf die GPU:** Zwei Bilder sind gleichzeitig in Arbeit. Freigaben
  folgen einer Timeline-Semaphore. Das Ende eines Bildes geht als sync_file an KMS
  (IN_FENCE_FD) und in die dmabufs, die es gelesen oder geschrieben hat. Die ausstehenden
  Schreibvorgänge eines Client-dmabufs wartet die GPU ab, nicht die CPU (wie wlroots).
  Geteilte Bilder wechseln per Queue-Family-Übergabe von und zur Foreign-Queue. Nur
  lavapipes Kopie in einen Dumb-Buffer wartet, weil die CPU kopiert.
- **Explizite Synchronisation** (`linux-drm-syncobj-v1`, wie KWin, Mutter, wlroots, anvil;
  `syncobj.rs`), angeboten, wenn der Treiber ein eventfd an einem syncobj-Punkt signalisiert
  (Smithays Bedingung, wie Mutter). Mesas Vulkan-WSI nutzt es von selbst, wenn das Gerät keine
  Software-GPU ist und Timeline-Semaphoren als opaque fd exportiert (`wsi_common.c`; NVK ja,
  lavapipe nein). Ein Commit mit Acquire-Punkt wartet auf diesen statt auf den impliziten Fence
  seines dmabufs; für einen solchen dmabuf wartet die GPU nicht auf dessen ausstehende
  Schreibvorgänge und bekommt das Ende des Bildes nicht eingetragen – nouveau trägt jede
  Submission als Schreiber aller geteilten Puffer ein, die implizite Synchronisation wartete
  so auf spätere Arbeit der Clients (0.5.112). Lässt sich auf den Acquire-Punkt nicht warten
  (kein eventfd), bleibt für diesen Commit die implizite Synchronisation. Den Release-Punkt
  signalisiert Smithay, wenn die letzte Referenz auf den Puffer fällt; ein Bild hält die Puffer,
  die es las, bis die GPU es beendet hat, frühestens bis zum vblank des Bildes (KMS wartete auf
  seinen Fence) – ohne Bild unterwegs auch, wenn Clients „frame done“ ohne Bild hören –, wie
  KWin den Release-Punkt an den Render-Fence bindet. Ein erneut angehängter Puffer behält auch
  den neuen Release-Punkt bis dahin (Smithay ließe ihn liegen; wlroots bindet je Commit). Die
  Protokollfehler, die Smithay nicht prüft (Puffer ohne Punkte, Punkt ohne Puffer, shm mit
  Punkten), meldet Beize in wlroots' Reihenfolge. Clients ohne das Protokoll behalten die
  implizite Synchronisation. Kommt Direct Scanout, gilt ein Puffer auf einer Ebene erst mit dem
  Flip, der ihn ersetzt, als frei. In der VM nimmt lavapipe dmabufs mit Modifier über
  `/dev/udmabuf` (Kernel `UDMABUF`, wie Fedora); der Smoke-Test prüft Acquire, Release und
  Protokollfehler mit Dumb-Buffern von virtio-gpu.
- **Ohne Änderung kein Bild:** Smithays Damage-Tracker mit dem Alter jedes Bildes. Ist
  nichts geändert, gibt es weder Zeichnen noch Commit, und die Anzeige-Engine zeigt das
  letzte Bild weiter. macOS' „bis 10 Hz“ ist eine Eigenschaft des Panels (ProMotion), nicht
  des WindowServers; auf Laptops greift ohne Commits von selbst Panel Self Refresh. VRR
  (amdgpu, i915; nouveau kann es nicht) folgt mit dem Zeitgeber für Clients.
- **Gemessen** (21.10., VM, lavapipe, 1280x800, Hintergrundbild + 4 transparente Fenster):
  ganzes Bild 2,57 ms (GPU 1,60 ms), 80x40 Damage 0,22 ms (GPU 0,17 ms). Die GPU-Zeit
  misst der Renderer je Bild mit Timestamps, Grundlage für den Zeitplan „so spät wie möglich
  vor dem vblank“.
- **Geprüft** vom Desktop-Smoke (Pixelproben, Bildschirmfoto gegen QEMUs screendump).
  `beize-render-test` lief in keinem Test mehr und ist entfernt (25.10.).
- **Noch nicht:** `host_image_copy` (Upload ohne Staging, lavapipe und NVK ab Turing), Direct
  Scanout und Overlay-Ebenen (kommen in die Bildschleife vor dem Rendern), der dmabuf-Weg
  mit Modifiern (nur auf Hardware prüfbar).

### 8. Fenster, Eingabe, Zeiger (Schritt 2, umgesetzt 21.10.)

- **Protokolle:** wl_compositor, wl_shm, linux-dmabuf v4 (die GPU als Hauptgerät, Formate
  des Renderers), wl_seat, wl_output mit xdg-output, wl_data_device, xdg-shell. Bis die
  Shell Fenster anordnet (Schritt 4), füllt jedes Fenster den Bildschirm, das neueste liegt
  oben und hat die Tastatur. Popup-Grabs lehnt der Compositor ab, das Protokoll schließt
  dann das Popup (`popup_done`). Grabs kommen mit den Menüs von Beize UI.
- **Bildtakt:** „Frame done“ bekommt ein Client, wenn sein Bild auf dem Schirm ist (vblank),
  nicht wenn er committet. Ändert ein Commit nichts Sichtbares, kommt es eine
  Bildwiederholung später, so dreht kein Client leer. Gemessen in der VM: 120 Bilder im
  Abstand von 13,33 ms bei 75 Hz.
- **Tastatur:** das XKB-Layout, das localed in `/etc/vconsole.conf` ablegt (`XKBLAYOUT=`
  usw., vconsole.conf(5)). Gelesen wird die Datei direkt, ohne D-Bus-Client im Compositor.
  Ändert localed sie, gilt das neue Layout sofort (inotify auf der Datei selbst, wie systemd
  `/etc/localtime` beobachtet; ganz `/etc` weckte den Compositor bei jeder dort geschriebenen
  Datei), wie bei macOS ohne Abmelden.
  Wiederholung mit GNOMEs Vorgaben (500 ms, 33/s), bis die Einstellungen es anbieten.
  Strg+Alt+F1…F12 wechselt das VT über logind.
- **Zeiger:** auf der Cursor-Ebene, zwei Dumb-Buffer ARGB8888, so groß wie der Treiber
  meldet. Eine Bewegung ist ein Commit dieser Ebene, ohne Zeichnen (wie macOS, KWin,
  Mutter). Ein Bild kommt nur bei Änderung in den Puffer, der gerade nicht angezeigt wird.
  Ohne passende Ebene zeichnet der Compositor den Zeiger ins Bild. Eine Bewegung kostet
  dann zwei kleine Damage-Rechtecke (VM, lavapipe: 0,36 ms GPU). Das ist in VMs immer so:
  virtio-gpu zeigt seine Cursor-Ebene seit Linux 6.8 nur Clients, die den Hotspot setzen
  (`DRM_CLIENT_CAP_CURSOR_PLANE_HOTSPOT`), und Smithays KMS-Schicht kann das nicht.
  Vorschlag an Smithay liegt bei den anderen Berichten. Die Zeiger kommen aus dem Thema
  (ADR 0028 §8); ein Client-Cursor (shm) wird kopiert.

### 9. Beize UI und Terminal (Schritt 3, Entwurf 22.10.; Schriften, Start, Tasten vom Besitzer entschieden)

Ziel: das erste eigene Fenster mit scharfem Text, das Terminal als erste App. Fenster,
Titelleiste und Anordnung: ADR 0028.

- **Aufbau:** `os/beize-ui` (Bibliothek: Fenster, Eingabe; Text, Szenen und Titelleiste seit
  23.10. in `os/beize-draw`, das der Compositor für die Rahmen fremder Apps mitnutzt) und
  `os/beize-terminal` (App). Wayland über `wayland-client` mit libwayland (im Image für Mesa),
  gezeichnet mit Vulkan über `VK_KHR_wayland_surface`: Mesas WSI liefert dmabufs, Bildtakt
  (FIFO) und auf lavapipe shm. Vorbild: GTK 4 und Zed zeichnen ebenso über WSI. Die
  Vulkan-Grundlagen (Gerät, Quads, Texturen, Shader-Bau) teilen sich Compositor und Beize UI
  in einem gemeinsamen Crate, wie im Abschnitt 3 angelegt: `os/beize-vulkan` (22.10.), nur auf
  `ash`, ohne Smithay und Wayland; die Erweiterungen wählt der Aufrufer (Compositor: dmabufs
  und Fences, Beize UI: Swapchain), dazu Host-Puffer, Barrieren und das Aufzeichnen der Quads.
  Im Compositor bleiben dmabuf-Import, Sync-Files und die Geometrie in Smithays Koordinaten.
- **Fenster** (22.10., `os/beize-ui`): xdg-shell, Swapchain `B8G8R8A8_SRGB` (die GPU mischt in
  linearem Licht, Farben gibt man in sRGB an und Beize UI rechnet sie um), FIFO, undurchsichtig.
  libwayland und libvulkan per dlopen (wie Smithay libvulkan lädt; im Desktop für Mesa
  ohnehin da, der Buildroot der CI hat keine libwayland). Eigenes Rezept `beize-ui`, getrennt
  vom Compositor: im selben Cargo-Bau liefe sonst auch `beize-testclient` über libwayland.
  Geprüft mit `beize-ui-test` (Test-Medium) in der VM auf lavapipe: alle Farben bytegenau,
  halbes Weiß über `#1e1e1e` = 189 (linear gemischt; in sRGB wären es 143), Kanten
  pixelgenau.
- **Test Beize UI** (22.10., Smoke-Szenario `Desktop`): `beize-ui-test` zeigt eine Probe
  (Text in Inter und JetBrains Mono auf Dunkel und Hell, Farben, halbes Weiß); der
  Bildschirm muss genau der Referenz `os/beize-ui/tests/sample.png` gleichen, die dasselbe
  Programm offscreen zeichnet (`--offscreen DATEI 1280x800`), bis auf den Zeiger. Bei
  Abweichung legt `bz` das Bildschirmfoto als PNG und ein Differenzbild in die Logs; die
  Referenz ersetzt nur ein Mensch nach Ansicht. PNG wie openQA (Crate `png` in `bz`).
- **Text:** **Parley** (Layout, Shaping mit HarfRust, Schriftwahl mit Fontique), **Skrifa**
  (Umrisse und Hinting; Googles FreeType-Ersatz, seit Chrome 133 dort für Web-Schriften),
  gerastert von **zeno** (der Rasterizer von swash, mit dem Zed und COSMIC Glyphen zeichnen).
  Jede Glyphe wird einmal gerastert und liegt im Glyphen-Atlas der GPU (8 Bit Deckung), danach
  ist Text ein Quad je Glyphe, 1:1 auf ganzen Pixeln gelesen (`texelFetch`). Kantenglättung in
  Graustufen, keine Subpixel-Farbsäume (macOS seit 10.14). Hinting leicht (nur senkrecht,
  wie FreeType „light“): Inter behält seine Form, Grundlinien und Strichstärken sitzen auf dem
  Pixelraster; waagerecht Positionen in Viertelpixeln wie Chrome, für gleichmäßige Abstände.
  Verworfen: FreeType (C, Chrome ersetzt es gerade), swash als Ganzes (eigener Schriftleser
  neben Skrifa), vello_cpu/glifo (0.3, Glyphen-Cache noch experimentell; Kandidat für
  Vektor-Icons mit der Shell).
  Umgesetzt (22.10.): Parleys Version von Skrifa (0.44) mit `SmoothMode::Light` (Auto-Hinter
  für Schriften ohne eigenes, Vorschübe unverändert); zeno braucht `offset` und
  `render_offset` zusammen, sonst kommt das Viertelpixel nicht an. Atlas 1024² R8, voll
  beginnt er neu. Gemischt wird in linearem Licht, mit **Skias Kontrast** für Text
  (`SkMaskGamma.cpp`, `SK_GAMMA_CONTRAST` 0,5): Deckung c' = c + (1 − c) · k · c mit
  k = 0,5 · linear(1 − Helligkeit der Textfarbe). Ohne ihn wirkte dunkler Text auf Hell dünner
  als heller auf Dunkel (Besitzer, 0.5.98; 0.5.99 „normal und gut so“). Verworfen: FreeTypes
  „stem darkening“ (verdickt jede Farbe, der Unterschied bliebe), DirectWrites Korrektur (für
  Mischen im Gamma-Raum gebaut).
- **Layout:** **Taffy** (Flexbox/Grid; Zed nutzt dieselbe Version 0.15). Im Terminal kaum
  gebraucht, aber die Grundlage jedes Widgets ab Schritt 4.
- **Schriften:** Inter 4.1 und JetBrains Mono 2.304, beide variabel, OFL, unter
  `/usr/share/fonts`, aus den offiziellen Release-Dateien mit fester Prüfsumme (Daten wie
  `linux-firmware`; aus den Quellen bräuchte es fontmake mit Python im Buildroot). Die
  Dateien am Release-Tag sind byte-gleich mit denen im Release-Archiv; Inters Kursive gibt es
  nur im Archiv und kommt, wenn Beize UI kursiv setzt (mit Zip in `bz`). Umgesetzt 22.10.,
  ebenso die Compose-Tabellen (`libx11-compose`). Ohne fontconfig: Beize UI liest
  `/usr/share/fonts` selbst und trägt die Schriften bei Fontique ein (Inter für sans-serif und
  system-ui, JetBrains Mono für monospace), wie CoreText die Systemschriften von macOS kennt.
  Fontiques eigene Suche (`system`) ist unter Linux fontconfig, sie bleibt aus. fontconfig
  kommt mit fremden Apps (M4, „eine zentrale Schriftkonfiguration“). Fehlende Zeichen (Emoji, CJK) zeigen bis zu einer Fallback-Schrift
  ein Kästchen; Auswahl und Größe (Noto, Emoji als COLRv1) mit der Shell.
- **Terminal** (22.10., `os/beize-terminal`, im Image `/usr/lib/beize/beize-terminal`, gebaut
  im Rezept `beize-ui`): **alacritty_terminal** (Zustandsmaschine der Escape-Sequenzen,
  Raster, Scrollback, Auswahl; Apache-2.0; Zeds Terminal baut darauf) unverändert aus
  crates.io. Eigen sind PTY, Zeichnen, Eingabe: alacrittys PTY-Start setzt der Shell immer
  `ALACRITTY_WINDOW_ID` und `WINDOWID` (Programme hielten Beize für Alacritty bzw. X11), und
  seine Ereignisschleife liest auf einem eigenen Thread. Beizes Terminal wartet in einem Thread
  im Poll des Fensters auf Compositor, PTY und Ende der Shell (pidfd); Beize UIs `dispatch`
  nimmt dafür fds der App. Verworfen: libghostty-vt (Zig im Buildroot), eigener Parser (Jahre
  an Sonderfällen). Die Shell ist die Login-Shell des Nutzers, als Login-Shell gestartet wie in
  macOS' Terminal, mit `TERM=xterm-256color` (wie VTE) und `COLORTERM=truecolor`. Dunkel
  (VISION, `#1e1e1e`), die 16 Farben aus GNOMEs Adwaita-Palette wie GNOME Console, 16–255
  nach xterm, Truecolor; 10 000 Zeilen Scrollback (wie GNOME Terminal). JetBrains Mono 14 px,
  eine Glyphe je Zelle aus der Zeichentabelle der Schrift, ohne Ligaturen (wie Alacritty,
  foot, VTE; `beize_ui::Cells`). Rahmen- und Blockzeichen zeichnet das Terminal selbst
  (VTE, kitty, foot), damit sie über Zeilen schließen; runde Ecken vorerst ohne Eckpixel,
  echte Bögen mit den Vektor-Icons der Shell. Maus: Auswahl (Doppelklick Wort, dreifach Zeile),
  Rad durch den Scrollback (auf dem zweiten Bildschirm als Pfeiltasten), Mausberichte an
  Programme (SGR und X10), Shift übergeht sie wie bei xterm. Kopieren und Einfügen über
  `wl_data_device` (Beize UI: Senden in einem kurzen Thread, Empfangen über eine Pipe im Poll);
  eingefügt wird ohne ESC und, wenn das Programm es will, in Klammern (bracketed paste);
  Programme dürfen kopieren (OSC 52), nicht lesen. Tasten Strg+Shift+C/V (wie GNOME
  Terminal, Strg+C bleibt Abbruch), endgültig mit dem Strg-Modell aus ADR 0028 §7.
- **Tastatur:** libxkbcommon mit Compose (22.10., `os/beize-ui/src/input.rs`). Die Keymap
  schickt der Compositor (wechselt das Layout, kommt eine neue), die Compose-Tabelle ist die
  der Locale (`LC_ALL`, `LC_CTYPE`, `LANG`; ohne passende die von `C.UTF-8`, Beize hat nur
  UTF-8-Tabellen, Daten aus libX11). Text mit Strg ist das Steuerzeichen (Strg+C = ETX), wie
  ein Terminal es weitergibt. Wiederholung macht das Fenster selbst nach `repeat_info`
  (Wayland); kommt sie zu spät, eine statt nachzuholen. Zeiger in Fensterkoordinaten,
  Scrollen je Zeiger-Frame in Pixeln und Rastungen (`axis_value120`). Geprüft: Tests mit dem
  deutschen Layout (Buildroot), in der VM `^` + `e` = ê, `^` + Shift+`o` = Ô, Wechsel von
  `de(nodeadkeys)` zu `de` per localectl ohne Neustart des Fensters.
- **Start** (22.10.): Strg+Alt+T öffnet jederzeit ein neues Terminal (wie Ubuntu; die Taste
  zählt wie auf einem geladenen lateinischen Layout, wie GNOME), der Desktop startet ohne
  (ADR 0028 §6). Jedes Fenster ist eine Instanz
  von `app-beize\x2dterminal@.service`, benannt nach systemds Desktop-Integration
  (`app-<App-ID>@<Zufall>.service`, Bindestriche maskiert), damit Werkzeuge die App eines
  Prozesses erkennen; nie ein Kind des Compositors (sonst erbte es seine Sandbox). Die Unit hängt
  an `graphical-session.target` (startet nach ihr, stoppt mit ihr). Bewusst ohne Sandbox: die
  Shell des Nutzers mit seinen Rechten, wie macOS' Terminal (`security.toml` 9,2; die Ratsche
  erfasst jetzt auch `app-*`-Units). Der Compositor ruft `StartUnit` in einem kurzen Thread mit
  eigener Verbindung, so wartet die Bildschleife nie auf den Manager. Den sd-bus-Code teilen
  Compositor und beize-session im Crate `os/beize-systemd`. Tasten, die der Compositor nimmt,
  behält er auch beim Loslassen (wie anvil und niri). Geprüft im Smoke-Szenario `Desktop`: kein
  Terminal nach dem Start, je eins nach zweimal Strg+Alt+T, keins nach dem Abmelden
  (Gegenprobe: andere Taste → rot).
- **Test Terminal** (22.10., Smoke-Szenario `Desktop`): In das neue Terminal (nach
  Strg+Alt+T) tippt `bz` `/run/t/beize-terminal-sample`, sobald dessen Shell liest (readline
  schaltet das Terminal aus dem kanonischen Modus; vorher getippte Tasten zeigte der
  Terminaltreiber vor dem Prompt). Die Probe (`os/beize-terminal/tests/sample.sh`, auf dem
  Test-Medium) zeigt je Zeile Umlaute und ein kombinierendes Zeichen, Attribute, die 16
  Farben, Graustufen, Truecolor, Rahmen und Blöcke. Der Bildschirm muss
  `os/beize-terminal/tests/sample.png` gleichen, einem Bildschirmfoto eines Laufs (wie
  openQAs „Needles“), mit dem Zeiger, also ohne jede Abweichung. Wie openQA wartet `bz`
  bis zu 10 s, bis das Bild stimmt. Fehlt die Referenz oder weicht sie ab, liegt das Foto in
  den Logs; ersetzt wird sie nur von Hand nach Ansicht. Gegenprobe: ein „ü“ der Probe als
  „u“ → rot, 12 Pixel. Dazu der Hardware-Test mit `pacing:` (prüft den Zeitplan).
- **Noch nicht:** Barrierefreiheit (AccessKit mit den ersten Widgets), Eingabemethoden
  (`text-input-v3`), gebrochene Skalierung (Faktor 1 beim 27"-1440p des Besitzers; Protokoll
  mit den Einstellungen).

## Reihenfolge

1. **Mesa** in `images/desktop/`: NVK und lavapipe, dazu libdrm, Wayland, Vulkan-Loader.
   Kernel: nouveau, GSP-Firmware. ✓ 17.10. Was davon bleibt:
   - GSP 535.113.01 lädt nouveau neben 570.144 nie: `[files] omit` in
     `recipes/linux-firmware.toml` lässt es weg (59 MiB).
   - NVKs Compiler ist Rust, also ohne CET (ADR 0014): Jeder Prozess, der NVK lädt, läuft
     ohne Shadow Stack, bis Rust CET stabil erzeugt (Fedora ebenso). Mesa ist das erste C++
     im Image (libstdc++, ADR 0023).
   - **lavapipe** mit LLVM 23.1.3 als JIT: nur x86, ein `libLLVM.so` nur mit den Komponenten,
     die Mesa verlangt (77 MB), kein clang; `/usr` +34 MiB. Bau ~30 min auf dem Runner, nur
     bei neuem LLVM oder Buildroot. LLVMs C++ braucht bis 1,5 GiB je Compiler (das Rezept
     begrenzt die Jobs nach dem Speicher); CMake braucht `CMAKE_ASM_FLAGS` für das
     CET-Kennzeichen.
2. **Compositor-Gerüst** nach Abschnitt 6–8. ✓ 22.10. (Hardware-Test 0.5.97).
3. **Beize UI** und das Terminal nach Abschnitt 9. ✓ 23.10. (Hardware-Test 0.5.101).
4. **Shell:** nach ADR 0028 (Menüleiste, Dock, Fenster, Kontrollzentrum, Glas, Symbole).

## Konsequenzen

- Mehr eigener Code als bisher: Renderer, Toolkit, Shell. Das ist Beizes Kern laut VISION,
  der Rest bleibt Upstream.
- LLVM verlängert den Bau von null um rund 30 min auf dem Runner; sonst kommt es aus dem
  Cache wie der Kernel.
- lavapipe macht die Oberfläche in jedem Smoke-Test prüfbar, ohne GPU im Runner, und ist
  Rückfall für Rechner ohne Vulkan-Treiber in Beize (VMs ohne 3D, bisher Intel und AMD),
  wie WARP unter Windows.
- NVK folgt dem Kernel von selbst; die Kernel-Version hängt nicht an NVIDIA.
- Kein GBM, kein OpenGL im Image. Dafür eigener Code für Framebuffer-Export und Bildschleife,
  die Smithays `DrmCompositor` sonst mitbrächte.
- `beize-gpu-probe` liegt im Tag `measure/nvk-0.5.92`: nach großen Mesa- oder
  Kernel-Sprüngen dieselbe Messung wiederholen.
