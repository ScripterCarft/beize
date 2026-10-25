# 0029 – Bildschirme: an- und abstecken, Verbindung neu aufbauen, später mehrere

Status: Angenommen · 2026-10-25 (mit dem Besitzer besprochen)

## Kontext

Der Compositor nimmt beim Start den ersten verbundenen Anschluss (`output.rs`, `surface`) und
behält ihn bis zur Abmeldung. Er hört nicht auf udev und liest die Eigenschaft `link-status`
nicht. Belegt im Code; die Folgen sind erschlossen und auf Hardware noch nicht geprüft:

- Monitor aus und wieder an, KVM-Umschalter, Kabel ab und an: DisplayPort meldet das als
  Trennen und Verbinden. Muss die Verbindung neu trainiert werden, setzt der Kernel
  `link-status` auf BAD und erwartet einen neuen Moduswechsel vom Compositor („userspace
  should re-train the link by performing a modeset“, Kernel-Doku *Standard Connector
  Properties*). Ohne ihn bleibt der Schirm schwarz bis zur nächsten Anmeldung.
- Ein später angesteckter Bildschirm bleibt dunkel; ein Laptop mit Monitor zeigt nur einen.

Heute gehören `DrmDevice`, Vulkan-Renderer und Allokator zu dem einen Bildschirm (`Output`,
`gpu.rs`). Mehrere Bildschirme einer GPU teilen sich aber Gerät und Renderer.

Vorbilder: **Mutter** und **KWin** lesen bei jedem udev-„change“ des DRM-Geräts die Anschlüsse
neu und setzen bei `link-status` BAD denselben Modus neu. **macOS** behält Fenster, wenn ein
Bildschirm geht, holt sie auf die verbleibenden und zeigt nach dem Ruhezustand des Monitors
ohne Zutun wieder alles.

## Entscheidung

1. **Aufbau:** eine GPU (`DrmDevice`, Renderer, Allokator) und eine Liste von Bildschirmen, je
   Anschluss mit CRTC, Swapchain, Damage-Tracker, Cursor-Ebene, Zeitplan (`schedule`) und
   Messung (`pacing`). Bis zum ADR-Abschnitt über mehrere Bildschirme zeigt Beize einen: den
   internen eines Laptops (eDP) zuerst, sonst den zuerst gefundenen.
2. **udev:** Der Compositor hört auf die Ereignisse des DRM-Geräts (Smithays `UdevBackend`,
   im schon eingeschalteten `backend_udev`) und liest bei jedem die Anschlüsse neu.
3. **Ein Bildschirm geht:** Seine Bildschleife ruht, Fenster behalten Ort und Größe, Clients
   hören „frame done“ im Takt verdeckter Fenster (1 s). Der `wl_output` bleibt, bis ein anderer
   ihn ersetzt (Apps sollen nicht ohne Bildschirm dastehen).
4. **Er kommt wieder oder ein anderer:** bester Modus wie beim Start, Modus setzen, alles neu
   zeichnen; Fenster, die nicht mehr passen, holt `windows::constrain` herein.
5. **`link-status` BAD:** denselben Modus erneut setzen (Neutraining), wie Mutter und KWin.
6. **Später, eigener Abschnitt:** mehrere Bildschirme gleichzeitig (Fenster verteilen,
   Menüleiste je Bildschirm wie macOS, Skalierung je Bildschirm nach EDID).

## Prüfung

- **Smoke (Desktop):** `echo off > /sys/class/drm/<anschluss>/status` trennt den Anschluss,
  ein `change` des udev-Geräts der GPU (`udevadm trigger`) meldet es wie ein Hotplug.
  virtio-gpu meldet das Trennen nicht selbst: der Kernel feuert beim Abfragen nur für
  Treiber mit Polling ein Ereignis (`drm_helper_probe_single_connector_modes`). Der
  Compositor meldet den Bildschirm als weg, das Bild ist dunkel, das Fenster bleibt;
  `echo detect`: der Bildschirm zeigt das Fenster an seinem Platz (Pixelprobe). Zweimal, die
  zweite Runde im selben Modus. Gegenproben (25.10.): ohne das Loslassen bleibt das Fenster
  sichtbar, ohne das Neu-Nehmen kommt der Bildschirm nicht wieder. Dass der Compositor nach
  der Rückkehr selbst zeichnet, ist in der VM nicht isolierbar (Clients committen laufend).
- **Hardware-Test:** neuer Teil in `measure/hw-test.sh`: Monitor aus und wieder an, Kabel ab
  und an. `link-status` lässt sich nur dort prüfen.

## Konsequenzen

- Umbau von `gpu.rs`/`output.rs`/`state.rs` (GPU und Bildschirm getrennt), geschätzt
  300–400 Zeilen; keine neue Abhängigkeit.
- Der Weg zu mehreren Bildschirmen ist frei, ohne ihn jetzt zu gehen.
