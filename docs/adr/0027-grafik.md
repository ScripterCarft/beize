# 0027 – Grafik: Compositor, Vulkan, Toolkit, NVIDIA, Schrift

Status: Angenommen · 2026-10-17 (mit dem Besitzer besprochen; NVIDIA nach der Messung in
Schritt 1: NVK)

## Kontext

M3 bringt die Oberfläche. `docs/VISION.md` lässt offen: Grafik-API des Compositors, UI-Toolkit,
Systemschrift, Icon-Stil, NVIDIA-Treiber (#17). Maßstab ist macOS: alles Sichtbare aus einem
Guss, vom System selbst gebaut. Feste Regeln von Beize, die hier hineinspielen: immer der
neueste stabile Kernel (zurzeit 7.2.10), keine fremden Patches, alles aus dem Quellcode und
signiert, nur der Desktop bekommt Grafik (ADR 0025). Zielhardware: RTX 3070 Ti; der Monitor
des Besitzers kann 1440p bis 143,9 Hz (240 Hz nur in 1080p). Maßstab bleibt das strengere
Budget der VISION: 4,2 ms je Frame (240 Hz), Shell < 1 ms GPU-Zeit.

Stand der Bausteine (Oktober 2026):

- **Smithay** bringt Wayland-Protokoll, DRM/KMS, GBM, libinput, Puffer und Damage-Tracking.
  Eingebaut sind nur GLES-, glow- und pixman-Renderer, ein Vulkan-Renderer existiert nur in Forks und fremden
  Crates (COSMIC plant einen, Otto und ein niri-Fork haben experimentelle).
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
(4,2 ms). Den NVIDIA-Testbau gibt es nicht: Er hätte `bz` Fremdmodule signieren lassen, und
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

## Reihenfolge

1. **Mesa** in `images/desktop/`: NVK, dazu libdrm, Wayland, Vulkan-Loader; lavapipe
   (braucht LLVM im Image) mit Schritt 2. Kernel: nouveau, GSP-Firmware. Messung nach
   Abschnitt 4. Umgesetzt 2026-10-17:
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
2. **Compositor-Gerüst:** KMS, libinput, Vulkan-Renderer, ein Fenster eines Testclients.
   Smoke-Test mit lavapipe, Screenshot-Vergleich.
3. **Beize UI:** Text, Layout, erstes eigenes Fenster; das Terminal als erste App.
4. **Shell:** Taskleiste (Dock- oder Windows-Stil vorher entscheiden), Glas, Icons.

Mit dem ersten C++-Programm (Mesa/LLVM) den Demangler von elfutils einschalten (ADR 0023).

## Konsequenzen

- Mehr eigener Code als bisher: Renderer, Toolkit, Shell. Das ist Beizes Kern laut VISION,
  der Rest bleibt Upstream.
- LLVM macht den Bau von null deutlich länger; die CI baut es nur bei Änderungen
  (Cache wie beim Kernel).
- lavapipe macht die Oberfläche in jedem Smoke-Test prüfbar, ohne GPU im Runner.
- NVK folgt dem Kernel von selbst; die Kernel-Version hängt nicht an NVIDIA.
- `beize-gpu-probe` liegt im Tag `measure/nvk-0.5.92`: nach großen Mesa- oder
  Kernel-Sprüngen dieselbe Messung wiederholen.
