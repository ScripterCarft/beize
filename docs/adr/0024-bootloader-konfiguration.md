# 0024 – Bootloader-Konfiguration kommt mit jeder Version

Status: angenommen · 2026-10-15 (Besitzer: Punkte 1–3 freigegeben, im Chat vom 15.10.)

## Kontext

Startet der Besitzer über das Menü von systemd-boot (Leertaste, auch jede Wiederherstellung),
läuft die Konsole danach in 1024×768 statt 2560×1440. Belegt im Journal von 0.3 auf dem PC:
Menü-Start `Console: switching to colour frame buffer device 128x48` (Schrift 8×16), Start
ohne Menü `160x45` (Schrift 16×32). Ursache: `console-mode keep` – das Menü schaltet die
Text-Konsole der Firmware ein, deren Modus auf diesem Board 1024×768 ist; simpledrm übernimmt
den Grafikmodus, den die Firmware dem Kernel übergibt.

Tiefer liegt: `loader.conf` schrieb nur `bz image` bei der Installation. Updates (ADR 0020)
erneuern UKI und Bootloader, nie seine Konfiguration – eine Änderung wie diese erreichte
keinen installierten Rechner.

Etablierte Praxis: macOS bringt die Startkonfiguration mit jedem Update auf das
Preboot-Volume, sie gehört zur Systemversion. systemd liefert für Image-Systeme keinen
Mechanismus (`bootctl update` ersetzt nur den Bootloader, `bootctl install` schreibt
`loader.conf` nur, wenn sie fehlt).

## Entscheidung

- **`loader.conf` liegt in `/usr/lib/beize/loader.conf`** – eine Quelle, von dm-verity
  geschützt, mit Kommentaren zu jeder Zeile. `bz image` schreibt dieselbe Datei auf das ESP
  der Installation.
- **Nach einem erfolgreichen Start bringt `beize-boot-update.service` sie auf das ESP**,
  gleich nach dem Bootloader (`/usr/lib/beize/loader-conf`): nur wenn sie abweicht, daneben
  geschrieben und darübergeschoben wie bei bootctl. Erst nach `boot-complete.target`, aus
  demselben Grund wie der Bootloader: eine neue Version, die nicht startet, ändert am Start
  nichts. Rollt systemd-boot auf die alte Version zurück, bleibt die Datei der neuen bis
  zum nächsten erfolgreichen Update – sie enthält nichts Versionsgebundenes.
- **`console-mode max`**: das Menü stellt den höchsten Modus der Firmware ein, die
  Auflösung des Bildschirms. Starts ohne Menü berührt das nicht (systemd-boot schaltet den
  Modus nur, wenn es das Menü zeigt).
- **Der Startbericht nennt Anzeigemodus und Eintrag** (`display=`, `entry=`): den Modus,
  den die Firmware dem Kernel übergibt (`screen_info` in `/sys/kernel/boot_params/data`),
  und `LoaderEntrySelected`.

## Prüfung

Der Smoke-Test legt die `loader.conf` von 0.3 auf das ESP; der erste erfolgreiche Start
muss sie durch die aus `/usr` ersetzen. Das Herunterschalten selbst bildet OVMF nicht nach
(gemessen 2026-10-15: Menü-Start und normaler Start beide 1280×800) – die Auflösung prüft
der Hardware-Test von 0.4 im Journal (Menü-Start: `160x45`).

## Konsequenzen

- Jede künftige Änderung an `loader.conf` erreicht installierte Rechner mit dem nächsten
  Update, ohne Neuinstallation.
- Das ESP wird nur geschrieben, wenn sich die Datei ändert, nicht bei jedem Start.
- Eigene Änderungen an `/loader/loader.conf` überschreibt der nächste Start; Beize hat
  dafür keinen Platz vorgesehen (wie macOS).
