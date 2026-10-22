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

### 1. Compositor = Shell, auf Smithay

Ein eigener Compositor (`os/beize-compositor`), der zugleich die Shell ist: Fensterrahmen,
Taskleiste, Kontrollzentrum, Sperrbildschirm, Animationen. Smithay übernimmt Protokoll,
KMS und Eingabe und bleibt unverändert (Crate aus crates.io, keine Forks). Vorbild: macOS
WindowServer mit Dock aus einer Hand.

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
  des Targets, und nimmt die Umgebung zurück. Endet der Compositor, kehrt es zur Shell zurück
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
  rechnet `bz` aus, ±2 je Kanal, höchstens 24×24 Pixel Zeiger), keine Warnung außer
  Smithays bekannter, Abmelden stoppt alles. Bei Abweichung ein Differenzbild in den Logs.

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
- **Ohne Änderung kein Bild:** Smithays Damage-Tracker mit dem Alter jedes Bildes. Ist
  nichts geändert, gibt es weder Zeichnen noch Commit, und die Anzeige-Engine zeigt das
  letzte Bild weiter. macOS' „bis 10 Hz“ ist eine Eigenschaft des Panels (ProMotion), nicht
  des WindowServers; auf Laptops greift ohne Commits von selbst Panel Self Refresh. VRR
  (amdgpu, i915; nouveau kann es nicht) folgt mit dem Zeitgeber für Clients.
- **Gemessen** (21.10., VM, lavapipe, 1280x800, Hintergrundbild + 4 transparente Fenster):
  ganzes Bild 2,57 ms (GPU 1,60 ms), 80x40 Damage 0,22 ms (GPU 0,17 ms). Die GPU-Zeit
  misst der Renderer je Bild mit Timestamps, Grundlage für den Zeitplan „so spät wie möglich
  vor dem vblank“.
- **Geprüft** von `beize-render-test` in der VM (über das Test-Medium von `bz`, nicht im
  Image): feste Szenen offscreen, pixelgenau gegen eine Referenz der CPU. Die
  Transformationen rechnet die Referenz mit Smithays eigener Funktion, nicht mit der des
  Renderers.
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
  Ändert localed sie, gilt das neue Layout sofort (inotify), wie bei macOS ohne Abmelden.
  Wiederholung mit GNOMEs Vorgaben (500 ms, 33/s), bis die Einstellungen es anbieten.
  Strg+Alt+F1…F12 wechselt das VT über logind.
- **Zeiger:** auf der Cursor-Ebene, zwei Dumb-Buffer ARGB8888, so groß wie der Treiber
  meldet. Eine Bewegung ist ein Commit dieser Ebene, ohne Zeichnen (wie macOS, KWin,
  Mutter). Ein Bild kommt nur bei Änderung in den Puffer, der gerade nicht angezeigt wird.
  Ohne passende Ebene zeichnet der Compositor den Zeiger ins Bild. Eine Bewegung kostet
  dann zwei kleine Damage-Rechtecke (VM, lavapipe: 0,36 ms GPU). Das ist in VMs immer so:
  virtio-gpu zeigt seine Cursor-Ebene seit Linux 6.8 nur Clients, die den Hotspot setzen
  (`DRM_CLIENT_CAP_CURSOR_PLANE_HOTSPOT`), und Smithays KMS-Schicht kann das nicht.
  Vorschlag an Smithay liegt bei den anderen Berichten. Beize hat noch kein Cursor-Thema
  (Icons kommen mit der Shell), jeder benannte Cursor ist bis dahin Beizes Pfeil. Ein
  Client-Cursor (shm) wird kopiert.
- **Geprüft** in der VM mit `beize-testclient` (Test-Medium, nicht im Image) und den Tasten
  und Mausbewegungen von `bz debug`: Fenstergröße, Layout `de(nodeadkeys)` und der Wechsel
  zu `us` per localectl, Tasten, Zeiger, Klick, VT-Wechsel hin und zurück, 0 % CPU im
  Leerlauf. Die Cursor-Ebene selbst prüft erst der Hardware-Test.

## Reihenfolge

1. **Mesa** in `images/desktop/`: NVK und lavapipe, dazu libdrm, Wayland, Vulkan-Loader.
   Kernel: nouveau, GSP-Firmware. Messung nach Abschnitt 4. Umgesetzt 2026-10-17:
   - mesa-clc (übersetzt NVKs OpenCL-C beim Bau) läuft auf der Baumaschine mit LLVM 22 aus
     dem Buildroot; im Image ist kein LLVM. NVK baut in 50 s. Rust in Meson-Projekten
     über die Cross-Datei von `bz`; Mesas Crates prüft `bz` gegen Mesas Wraps
     (`latest = { pinned-by = … }`).
   - Firmware: `bz` liefert genau, was Module anfordern (ADR 0015), Links wie in
     linux-firmware' WHENCE, unkomprimiert (EROFS komprimiert). GSP 535.113.01 fordert
     nouveau nur als Rückfall an und lädt es neben 570.144 nie: `[files] omit` in
     `recipes/linux-firmware.toml` lässt es weg (59 MiB).
   - NVKs Compiler ist Rust, also ohne CET (ADR 0014): Jeder Prozess, der NVK lädt, läuft
     ohne Shadow Stack, bis Rust CET stabil erzeugt. Fedora hat denselben Stand.
   - Mesa ist das erste C++ im Image: libstdc++ und der Demangler von elfutils (ADR 0023).
   - **lavapipe** mit LLVM 23.1.3 als JIT: nur das x86-Backend, ein `libLLVM.so` wie bei
     Fedora, aber nur mit den Komponenten, die Mesa verlangt (77 statt 94 MB), kein clang,
     keine Werkzeuge. `/usr` wächst um 34 MiB auf 152 MiB. Bau lokal 10 min, auf dem
     CI-Runner (4 Kerne) geschätzt 30 min, nur bei neuem LLVM, neuem Buildroot und im wöchentlichen Lauf von null. Zwei Stolpersteine:
     LLVMs C++ braucht bis 1,5 GiB je Compiler, das Rezept begrenzt die Zahl nach dem
     Speicherlimit (24 auf 16 GiB liefen in den Speicherdruck, 45 min je Datei); CMake
     übergibt `CFLAGS` nicht an den Assembler, ohne `CMAKE_ASM_FLAGS` fehlte BLAKE3s
     Assembler das CET-Kennzeichen und damit libLLVM. Mesa findet LLVM über seinen
     dokumentierten „binary wrap“ statt `llvm-config`, das auf der Baumaschine nicht läuft.
     Geprüft: Compute-Shader über den JIT mit den Bibliotheken des Images („llvmpipe (LLVM
     23.1.3, 256 bits)“, 65 536 von 65 536 Ergebnissen richtig); in der VM prüft es der
     Smoke-Test des Compositors (Schritt 2).
2. **Compositor-Gerüst** nach Abschnitt 6: libseat und libinput (mit libevdev, mtdev) im
   Desktop; Probe dmabuf → KMS in der VM; `os/beize-compositor` mit Vulkan-Renderer für
   Smithays `Renderer` (shm- und dmabuf-Texturen, Damage), xdg-shell, ein Fenster, Cursor,
   Tastatur nach localeds Layout; `beize-session`; Smoke-Szenario mit Screenshot-Vergleich.
   Danach Hardware-Test beim Besitzer: NVK mit 144 Hz, Maus, Tastatur, Bildabstand.
3. **Beize UI:** Text, Layout, erstes eigenes Fenster; das Terminal als erste App.
4. **Shell:** Taskleiste (Dock- oder Windows-Stil vorher entscheiden), Glas, Icons.

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
