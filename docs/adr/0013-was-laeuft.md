# 0013 – Was läuft, entscheidet das Image

Status: angenommen · 2026-10-10

## Kontext

systemd aktiviert Units über Presets: Beim ersten Start schreibt es für jede Unit,
die eine Preset-Regel einschaltet, Links nach `/etc`. Ohne Preset-Datei gilt „alles
an“ (Debian-Stil); Fedora liefert deshalb `disable *` und eine geprüfte Liste. Beize
lieferte kein `disable *`: Jede Unit mit `[Install]` ohne Regel lief, ohne dass Beize
das entschieden hatte (`e2scrub_reap`, `e2scrub_all.timer`, `systemd-oomd`).

Presets sind für Paketinstallationen gedacht (systemd `docs/PRESET.md`). Ein
Image-System installiert keine Pakete: Links in `/etc` würden bei einem A/B-Update
nicht mitwandern – ein neuer Dienst bliebe aus, ein entfernter hinterließe tote
Links. Außerdem war `systemd-boot-update.service` aktiv: Es kopiert den Bootloader aus
`/usr` auf die ESP, sobald `/usr` einen neueren hat – den unsignierten, während die
ESP den mit Beizes Schlüsseln signierten trägt. Das erste Update mit neuem systemd
hätte den Start unter Secure Boot verhindert.

## Entscheidung

- **Was läuft, ist Teil des signierten Images** – wie bei macOS, wo nur die Dienste
  des versiegelten Systemvolumes starten. `bz` wendet die Presets beim Bauen an, mit
  dem `systemctl` aus Beizes eigenem systemd-Build (gleiche Version wie im Image), und
  legt die entstehenden Links nach `/usr/lib/systemd/{system,user}`. So macht es
  upstream-systemd mit seinen eigenen Kerndiensten. Ein Update ändert sie atomar mit.
- **Regeln, in dieser Reihenfolge** (die erste passende gilt):
  1. `images/<variante>/system.preset` – Beizes eigene Units und Abweichungen, jede mit
     Grund, die der Basis zuerst (ADR 0025); Benutzer-Units für alle Benutzer:
     `user.preset` daneben, `systemctl --global`;
  2. die Vorgaben von systemd (`90-systemd.preset`) – das, was die systemd-Entwickler
     testen; davon abzuweichen braucht einen konkreten Grund;
  3. `disable *` – alles andere ist aus.
- Abweichungen stehen mit Grund in `images/base/system.preset`, z. B. `systemd-boot-update.service`
  aus (siehe Kontext): Den signierten Bootloader aus `/usr` installiert Beizes
  `beize-boot-update.service` nach einem gelungenen Start (ADR 0020).
- Ausgeliefert wird nur `99-beize.preset` mit `disable *` (System und Benutzer): Der
  Preset-Durchlauf beim ersten Start schreibt nichts nach `/etc`. `/etc` enthält damit
  nur, was der Besitzer selbst ändert; abschalten kann er jede Unit mit `systemctl
  mask` (später über die Einstellungen-App).
- **Grenzen für Journal und Coredumps:** die Standardwerte von systemd – höchstens 10 %
  der Platte und 4 GiB, Coredumps zusätzlich nach Alter gelöscht. Sie sind fest und
  passen sich der Platte an; eigene Zahlen wären ohne Messung willkürlich.

## Konsequenzen

- `bz smoke` verlangt beim zweiten Start genau die aktiven Units aus
  `images/*/running-units.txt` (Dienste, Sockets, Timer, Pfade, nach Zweck gruppiert)
  und keinen Unit-Link in `/etc`. Taucht mit einer neuen systemd-Version eine Unit
  auf oder verschwindet eine, ist der Test rot, bis Beize das entschieden hat.
- systemd-Vorgaben ohne Nutzer in Beize (Container, Erweiterungen, Updates, lokale
  Systemberichte) lauschen nur auf ihrem Socket. Sie werden Teil eines Features
  (M3–M5) oder mit Grund abgeschaltet.
- `systemd-oomd` ist aus, weil systemd es nicht vorgibt. Fedora schaltet es zusammen
  mit eigenen Regeln für den Speicherdruck ein; Beize tut das mit dem Compositor,
  wenn „die Vordergrund-App hat Vorrang“ umgesetzt wird.
- Ob ein Start mit fehlgeschlagener Unit als gescheitert zählt
  (`systemd-boot-check-no-failures`), wird mit den A/B-Updates entschieden.
