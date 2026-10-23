# 0017 – Tastatur: XKB-Layout, Konsolen-Keymap, Recovery-Key in der initrd

Status: angenommen · 2026-10-11

## Kontext

Beize kannte keine Tastaturbelegung: Konsole, Setup, Passwort und Recovery-Key liefen
mit der US-Belegung des Kernels. Auf einer deutschen Tastatur liegen damit y/z, Umlaute
und Sonderzeichen falsch, und das Passwort wird mit einer Belegung getippt, die der
Besitzer nicht sieht.

Beim Recovery-Key ist das ein Sicherheitsproblem. systemd-cryptsetup vergleicht ihn
Zeichen für Zeichen, also mit Bindestrichen. Die Modhex-Buchstaben liegen auf QWERTY,
QWERTZ und AZERTY an derselben Stelle, der Bindestrich nicht: Mit US-Belegung gibt die
Bindestrich-Taste einer deutschen Tastatur „/“, die einer französischen „6“. Upstream
setzt voraus, dass die initrd die Belegung des Rechners kennt; dracut und mkosi-initrd
kopieren dafür `/etc/vconsole.conf` hinein. Das ist bei Beize nicht möglich, und die
Ursache sind eigene Entscheidungen: Die initrd ist ein generisches, signiertes UKI
(ADR 0003, 0016), und Credentials auf der ESP sind gesperrt (ADR 0010). Damit kommt die
Belegung des Besitzers nie in die initrd. Dasselbe traf das Bestätigungswort beim
ersten Start („saved“: Das „a“ liegt auf AZERTY woanders).

## Entscheidung

- **Das Layout ist ein XKB-Layout** (xkeyboard-config, libxkbcommon): das, was später
  der Compositor nutzt. localed prüft jedes Layout gegen XKB (`-Dxkbcommon=enabled`).
- **Die Konsolen-Keymap dazu kommt aus systemds `kbd-model-map`, die Keymaps sind
  kbds eigene**, wie bei Arch Linux. Nicht übernommen wurde Fedoras Weg, Keymaps aus XKB
  umzuwandeln. kbds neue Umwandlung (`loadkeys --xkb-*`, 2.10.0) legt die AltGr-Ebene
  auf die Alt-Taste; upstream ist das nach dem Release behoben (0bb93f7), zusammen mit
  rund 20 weiteren Korrekturen. Fedoras `ckbcomp` (console-setup, Perl) hat keine
  signierten Quellen. Sobald ein kbd-Release die Korrekturen enthält, wird umgestellt.
- **beize-setup fragt das Layout als Erstes** und schaltet sofort um, damit Name und
  Passwort mit der eigenen Tastatur getippt werden. Angeboten werden nur Zeilen der
  Tabelle, deren Keymap es gibt; die Tabelle nennt auch Keymaps, die kbd nicht
  ausliefert (`ko`, `khmer`, `ara` …). Gesucht wird nach Name, Land oder Code
  („German“, „de“, „de(nodeadkeys)“); vorgeschlagen wird das Layout der Firmware
  (`LoaderKeyboardLayout` von systemd-boot). Gesetzt werden beide Hälften der Zeile über
  localed (`--no-convert`: nichts wird geraten). Danach wartet beize-setup, bis
  `systemd-vconsole-setup` die Keymap geladen hat; localed stößt das nur an.
- **Die Keymap des Besitzers liegt für die initrd in einer EFI-Variable**, so wie macOS
  das Tastaturlayout für die FileVault-Abfrage im NVRAM hält.
  - `BeizeKeymap-e6e221d0-db73-4fa8-99da-ba41e1b78888` (nicht flüchtig, beim Start und
    im System lesbar), Inhalt: der Name der Keymap.
  - Im System: `beize-keymap.path` beobachtet `/etc/vconsole.conf`, `beize-tpm keymap
    save` schreibt die Variable, und zwar nur bei einer Änderung.
  - In der initrd: `beize-tpm keymap load` schreibt sie vor `systemd-vconsole-setup`
    nach `/etc/vconsole.conf`. Die Konsolen-Abfrage und das Notfall-Tor warten auf
    `systemd-vconsole-setup`.
  - Die Variable wird nicht gemessen. Angenommen wird nur der Name einer Keymap, die
    die initrd hat (Buchstaben, Ziffern, `.`, `_`, `-`); sonst bleibt US. Wer EFI-Variablen
    schreiben kann, erreicht damit höchstens eine Tastatur, die falsche Zeichen tippt.
  - Ohne Variable, also beim ersten Start, gilt das Layout der Firmware, sonst US.
- **Der Recovery-Key wird getippt, wie er angezeigt wird**, mit Bindestrichen und mit
  der eigenen Tastatur. Das Notfall-Tor nimmt ihn in jeder Form an: Nur seine 64
  Buchstaben zählen, gleich was dazwischen steht. Die Abfrage von systemd-cryptsetup
  selbst bleibt unverändert (kein eigener Agent, kein Patch).
- **Das Bestätigungswort beim ersten Start ist „stored“**: nur Buchstaben, die auf
  QWERTY, QWERTZ und AZERTY gleich liegen, weil die Keymap da noch nicht bekannt ist.
- **Locale:** LANG bleibt C.UTF-8. Übersetzungen gibt es noch keine; regionale Formate
  und glibc-Locales kommen mit Onboarding und Einstellungen (M3+). C.UTF-8 selbst liefert
  das glibc-Paket (`/usr/lib/locale/C.utf8`, wie Fedora und Debian; seit glibc 2.35 nicht
  mehr eingebaut, 23.10. nachgeholt: ohne sie fiel jedes Programm auf ASCII zurück, bash las
  ein getipptes „ê“ als Meta-Tasten). Konsolen-Logins lesen `/etc/locale.conf` in
  `/etc/profile` (wie Fedoras `lang.sh`), der Desktop hat sie vom Service-Manager.
- **Prüfungen im Smoke-Test:**
  - Das Setup antwortet „German“, nach Ctrl+C `de(nodeadkeys)`. Der Boot-Bericht zeigt
    die Keymap in `vconsole.conf`, was die Taste zwischen T und U auf tty1 ergibt (nur
    mit deutscher Keymap „z“) und den Inhalt der EFI-Variable.
  - In der initrd tippt der Test den Recovery-Key über QEMU als Scancodes auf der
    Tastatur: Der Bindestrich an der US-Stelle muss abgelehnt werden („ß“), die
    deutsche Bindestrich-Taste muss öffnen.
  - Das Notfall-Tor bekommt den Key mit „/“ statt Bindestrich.
  - Gegenproben: Setup mit „us“, initrd ohne gespeicherte Keymap, Tor ohne
    Normalisierung – jeweils rot.

## Konsequenzen

- Gefunden durch den Tastaturtest: Nach einem Vertipper am Recovery-Key band
  `beize-tpm reseal` mit dem ersten zwischengespeicherten Key neu, also dem falschen.
  Den TPM-Slot hatte es da schon gelöscht, denn das Löschen braucht keinen Key.
  Jetzt nimmt es den Key, der den LUKS-Header öffnet, und das vor dem Löschen.
- initrd 11,0 statt 10,8 MiB (loadkeys, PC-Keymaps, vconsole-setup).
- Ein Layout, das der Besitzer später mit `localectl set-x11-keymap` ohne `--no-convert`
  setzt, kann eine Keymap ergeben, die es nicht gibt (`kr` → `ko`). Die Einstellungen-App
  muss dieselbe Tabelle und Prüfung nutzen wie beize-setup.
- Für die Firmware-Sprachen ko-KR, km-KH und ar-SA nennt systemds Tabelle Keymaps, die
  kbd nicht hat. Ohne gespeicherte Keymap schlägt `systemd-vconsole-setup` dort fehl;
  bei Beize betrifft das nur die Zeit vor dem Setup. Das ist ein Fall für einen
  Upstream-Bericht an systemd.
- Wechselt der Besitzer die Tastatur, gilt in der initrd bis zur nächsten Änderung des
  Layouts die gespeicherte Keymap.
