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
(4,2 ms). **Offen:** nouveau meldete für 1440p keinen 240-Hz-Modus. Der Monitor (Dell
AW2723DF, DisplayPort) kann ihn und läuft unter Windows damit. Der Modus steht im DisplayID-Block
des EDID (1056,75 MHz) und passt mit 8 Bit je Farbe ohne Kompression (DSC) in HBR3 × 4
(25,4 von 25,9 Gbit/s). nouveaus Prüfung (`nv50_dp_mode_valid`) verwirft ihn genau dann, wenn
der Link nur HBR2 hat. Warum nouveau HBR2 annimmt, zeigt ein Diagnoselauf auf dem Rechner des
Besitzers (drm.debug: Raten von Monitor und Encoder).

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
  Shell als Benutzer-Unit führt. Kein Autostart über Shell-Profile.
- **Bildausgabe ohne GBM:** Atomares KMS über Smithays `DrmSurface`. Die Bilder des
  Bildschirms kommen aus Vulkan (`VulkanAllocator`, `VK_EXT_image_drm_format_modifier`, mit
  den Modifiern, die KMS für die Ebene meldet) und gehen als dmabuf an KMS. Den Schritt
  dmabuf → KMS-Framebuffer (`ExportFramebuffer`) und die Bildschleife (Ebenen, Cursor,
  Page-Flip) schreibt Beize selbst, statt Smithays `DrmCompositor` zu nutzen. Begründung: GBM
  käme in Mesa nur mit einem Gallium- bzw. OpenGL-Treiber für nouveau, also genau dem zweiten
  Grafikstapel, den Abschnitt 2 ausschließt. Vorbild: Valves gamescope (Steam Deck)
  allokiert seine Ausgabebilder ebenfalls in Vulkan. Die Bildschleife ist ohnehin Beizes Kern
  (Direct Scanout, Pacing).
- **VM:** Ob virtio-gpu einen dmabuf von lavapipe annimmt, klärt eine Probe vor dem Code.
  Falls nicht, kopiert der Compositor in der VM das fertige Bild in einen Dumb-Buffer
  (Smithay hat dafür einen Exporter). Der Direktweg wird dann nur auf echter Hardware geprüft.
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
  vollen CI-Stufe.

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
