# Beize – Vision

Arbeitstitel „Beize“. Stand 2026-10-07. Original (lebendes Dokument, nur mit
Login lesbar): https://claude.ai/code/artifact/1204e9ff-abc6-44b7-8320-17b195f5a226 –
diese Datei ist die verbindliche Kopie im Repo. Wo sich Entscheidungen seit dem
Original geändert haben, ist das mit **[aktualisiert]** markiert.

## Leitidee

Ein eigenes Betriebssystem auf dem Linux-Kernel, das sich anfühlt wie aus einem
Guss: **so konsequent wie macOS, so frei wie Windows, technisch so offen wie
Linux.** Keine Distribution mit anderem Theme – alles, was der Nutzer sieht und
anfasst, ist selbst entworfen und selbst gebaut. Vorbild ist Apples
Ökosystem-Gefühl: Shell, Einstellungen, Dateimanager, Dialoge und
Geräteverwaltung sprechen eine Sprache; nichts wirkt zusammengewürfelt oder wie
ein Server. Kernel, Treiber und bewährte Basisbibliotheken bleiben Upstream,
darüber gehört jede Zeile uns.

## Sieben Grundprinzipien (jede Entscheidung misst sich daran)

1. **Aus einem Guss.** Ein Design-System, eine Settings-App, ein Dateimanager, ein Dialog-Stil.
2. **Läuft ohne dich.** Updates, Aufräumen, Reparatur passieren selbst; Eingreifen möglich, nie nötig.
3. **Nachvollziehbar.** Jedes Byte auf der Platte hat einen sichtbaren Besitzer. Kein „Sonstiges: 100 GB“.
4. **Menschliche Sprache.** „AirPods Pro · 82 %“ statt „MSW__AirPodsQWE1X“. Keine internen Namen, Pfade, Fehlercodes in der UI.
5. **Schnell heißt flüssig.** Kein verpasster Frame, sofortiges Feedback. Gemessen wird Latenz, nicht Benchmarks.
6. **Frei wie Windows.** Jede App installierbar, alles abschaltbar, kein Walled Garden.
7. **Linux ist Technik, nicht Oberfläche.** Terminal und Config-Dateien existieren für Profis, aber kein Alltagsweg führt dorthin.

## Design-System

- **Shell hell, Apps dürfen dunkel sein** – pro Ebene gesteuert: Shell (Taskleiste,
  Kontrollzentrum, Suche, Benachrichtigungen, Login, Sperrbildschirm) und
  System-Apps (Einstellungen, Dateimanager, Dialoge, Installer) hell;
  Werkzeug-Apps (Terminal, Browser, Rider) dunkel; fremde Apps pro App wählbar,
  über das Portal an GTK/Qt/Electron gemeldet.
- **Material: modernes Glas, ruhiger als Apples Liquid Glass.** Drei Stufen statt Blur überall:
  - *Grundglas* (Fensterhintergründe, Seitenleisten, Taskleiste): einmal vorgeblurrtes Hintergrundbild wie Windows-Mica – fast kostenlos.
  - *Lebendiges Glas* (Kontrollzentrum, Suche, Menüs, Benachrichtigungen): Live-Blur nur dort, nur bei Änderung dahinter neu berechnet.
  - *Fläche* (Inhalte): leicht getönt mit feinem Korn, kein Blur.
  - Qualität ohne Rechenzeit: Farbanhebung (Sättigung/Aufhellung), feines Korn, 1-px-Lichtkante, Lichtbrechung nur in einem schmalen Randband, Schatten pro Fenstergröße gecacht.
  - Blur: Dual-Kawase auf ¼–⅛ Auflösung, Damage-Tracking, Direct Scanout bei Vollbild.
- **Performance-Budget:** 1440p @ 240 Hz → 4,2 ms pro Frame; die gesamte
  Shell-Komposition < 1 ms GPU-Zeit. Der Compositor misst laufend und senkt bei
  Knappheit die Blur-Auflösung statt einen Frame zu verpassen (auch mit VRR).
- Eine Systemschrift mit sauberem Hinting/HiDPI; federbasierte, unterbrechbare
  Animationen mit Ursprung (Fenster wächst aus seinem Icon); frei wählbare
  Akzentfarbe systemweit; eigener Icon-Satz, dezente abschaltbare Systemklänge.

## Systemmodell

- System unveränderlich und signiert, A/B mit automatischem Rückfall. **[aktualisiert:
  umgesetzt als nur-`/usr`-Image mit A/B-Slots, dm-verity und Boot-Counting – ADR 0007/0008.]**
- **Apps als Bundles** wie `.app` auf dem Mac: hinzufügen = installieren, löschen
  = deinstallieren, keine Reste. Daten jeder App nur in ihrem eigenen Bereich.
- **Speicheransicht:** jedes Byte einem Besitzer zugeordnet (System, App, Cache,
  Dokumente, Container, Papierkorb, Snapshots); Caches/Snapshots mit einem Klick frei.
- **Selbstpflege:** Caches, Logs, Snapshots mit festen Obergrenzen, räumen sich selbst auf.
- **Sicherheit:** Secure Boot mit eigenen Schlüsseln, signiertes UKI, dm-verity,
  verschlüsselte Daten mit TPM2. **[aktualisiert: die ganze Root-Partition
  (`/etc`, `/var`, `/home`) ist LUKS2-verschlüsselt, an TPM2 + Secure-Boot-Zustand
  gebunden, Recovery-Key beim ersten Start – ADR 0009.]**

## Shell (eine eigene Komponente)

| Element | Vorbild | Umsetzung |
| --- | --- | --- |
| Taskleiste | Windows 11 / macOS-Dock | schlicht, zentriert, angeheftete + laufende Apps, Status rechts; keine Widgets/Werbung. **Offen: Windows- oder Dock-Stil – eins von beiden, konsequent.** |
| Kontrollzentrum | macOS | Kacheln (WLAN, Bluetooth, Geräte, Lautstärke, Helligkeit, Fokus, Medien), aufklappbar z. B. Geräteliste mit Akku |
| Suche | Spotlight | ein Kürzel, ein Feld: Apps, Dateien, Einstellungen, Rechner, Befehle; lokal |
| Fenster | Windows Snap, macOS | Snap-Layouts, Alt+Tab mit Vorschau, virtuelle Desktops mit Gesten, eigene Rahmen vom Compositor |
| Benachrichtigungen | macOS | nach App gruppiert, Aktionen, Fokus-Modus |
| Dateimanager | Finder/Explorer | eigene App, zugleich Datei-Dialog aller Apps (Portal) |
| Einstellungen | macOS | eine App für alles, durchsuchbar; keine Einstellung nur per Datei |

## Geräte & Ökosystem

Eigener Geräte-Dienst übersetzt Bluetooth/USB/Netzwerk-Rohdaten in echte Namen,
Icons und Details. Maßstab AirPods: Modellname, Akku links/rechts/Case, Popup beim
Öffnen des Case, schnelles Wiederverbinden (ja); ANC/Transparenz (weitgehend,
Community-Protokoll); Apple-Gerätewechsel/iCloud (nein). Allgemein:
Gerätedatenbank, Akkustand für alles, Drucker treiberlos (IPP Everywhere),
Monitore mit Namen, Skalierung/Hz pro Monitor, HDR.

## Kompatibilität & Apps

Binärkompatibel zum Linux-Desktop-Standard (glibc, Wayland, Xwayland,
XDG-Portale); Design nie zulasten der Kompatibilität. Zielapps: Brave (Brave
Origin für Linux noch prüfen), Rider, .NET SDK + Aspire (braucht Podman/Docker),
1Password (inkl. SSH-Agent, Sperre an unseren Sperrbildschirm), Rust-Toolchain,
optional Flatpak. Entwickler-Werkzeuge in eigener, abgetrennter Schicht.
**Fremde Apps unauffällig kompatibel:** eine zentrale Schriftkonfiguration
(GTK/Qt/Electron/Java rendern identisch), eigenes Qt-Plattform-Plugin, GTK über
Portal, eigene Fensterrahmen wo erlaubt, Schatten/Ecken/Animationen für alle;
Datei-Dialog, Benachrichtigungen, Screensharing-Auswahl, Passwortabfragen und
Cursor kommen immer von Beize.

## Technik

Fünf Schichten: Oberfläche (Shell, Kontrollzentrum, Suche, Einstellungen,
Dateimanager, Terminal) · Compositor (Rust + Smithay, eigener Renderer, Effekte,
Fensterrahmen) · System-Dienste (Geräte-Dienst, Settings-Daemon, Updater, Portale,
App-Bundles) = **eigener Code**; Upstream-Basis (systemd, glibc, Mesa,
NVIDIA-Treiber, PipeWire, BlueZ, Xwayland) · Linux-Kernel = **Upstream, selbst gebaut**.
Zielwerte: Kaltstart bis Login < 5 s; kein verpasster Frame bei 240 Hz; Reaktion im
nächsten Frame; Vordergrund-App hat Vorrang (Scheduler/cgroups). GPU RTX 3070 Ti
(Ampere, offene NVIDIA-Kernelmodule) – Risiken Standby, VRR mit mehreren
Monitoren; eigener NVIDIA-Testpfad im Compositor.

## Nicht-Ziele

Kein eigener Kernel/Treiber · kein Windows/macOS-Klon (Konzepte, nicht Optik) ·
keine klassische Paketverwaltung für Nutzer · kein Theme-Baukasten (nur Akzent,
Hell/Dunkel, Hintergrund, Layout-Optionen) · keine beliebige Hardware (zuerst der
Rechner des Besitzers) · keine Telemetrie, Konten, Werbung.

## Entwicklung & Qualität

Wie ein Produkt: Monorepo, Rust-Workspace, CI baut und bootet, Tests auf jeder
Ebene (Unit, Boot in QEMU, später Screenshot-Vergleiche und Frame-Zeit-Regressionen),
ADRs, reproduzierbare Builds. **[aktualisiert: Produktionsschlüssel liegen im
1Password-Tresor „Beize“ des Besitzers und werden nur von ihm selbst genutzt –
Automatisierung/Claude greift nie auf 1Password zu (ADR 0006).]**

## Roadmap

M1 Bootet in QEMU (✓) → **M2 Echte Hardware (in Arbeit)** → M3 Compositor
(Fenster, Terminal, erste Effekte) → M4 Alltagstauglich (Rider, Aspire,
1Password, Audio, Standby; Wechsel zum Hauptsystem; App-Installationsmodell
vorher entscheiden) → M5 Feinschliff (Settings, Updater, Installer, Effekte).
Das jetzige Windows bleibt Hauptsystem bis M4; echte Tests auf der zweiten SSD.

**Offene Entscheidungen:** Name (Arbeitstitel Beize) · Taskleiste Windows- oder
Dock-Stil · Brave Origin unter Linux? · App-Bundle-Format eigen oder Flatpak-basiert ·
Systemschrift und Icon-Stil · Grafik-API des Compositors (Vulkan/GLES, Anfang M3) ·
UI-Toolkit für Shell/Einstellungen (Anfang M3).
