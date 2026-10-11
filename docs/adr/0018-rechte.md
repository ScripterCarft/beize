# 0018 – Rechte: run0 + polkit, kein setuid

Status: angenommen · 2026-10-11

## Kontext

Beize hat noch keinen Weg, als Besitzer Verwaltungsaufgaben zu erledigen: root meldet
sich an der Konsole mit demselben Passwort an wie der Besitzer (beize-setup kopiert
den Hash), `run0` liegt im Image, kann ohne polkit aber nur root selbst Rechte geben.

Der klassische Weg ist `sudo`: ein setuid-root-Programm, das in der Umgebung des
Aufrufers startet (Umgebungsvariablen, offene Dateien, Terminal, Ressourcenlimits) und
sich erst danach selbst einschränkt. Jede Lücke darin ist eine Rechteausweitung; sudo
und pkexec hatten genau solche (CVE-2021-3156 „Baron Samedit“, CVE-2021-4034 „PwnKit“).
`sudo-rs` und `doas` ändern die Sprache bzw. den Umfang, nicht das Modell.

Stand heute (geprüft in `build/work/usr-tree`): In `/usr` ist keine Datei setuid oder
setgid und keine hat Datei-Capabilities. Das ist Zufall der Rezepte
(`--disable-makeinstall-setuid`, Installation ohne root), nicht abgesichert.
`unix_chkpwd` und `dbus-daemon-launch-helper` liegen ohne setuid-Bit im Image und sind
damit wirkungslos.

## Entscheidung

- **Rechte über `run0`, nicht über sudo.** run0 bittet den Dienstmanager (systemd),
  den Befehl als root zu starten. Er läuft als frischer Dienst: saubere Umgebung,
  eigenes Terminal, im Journal sichtbar, kein Programm mit Sonderrechten auf der Seite
  des Aufrufers. systemd bringt run0 seit Version 256 genau als Ersatz für sudo mit;
  sudo bleibt draußen.
- **Ob jemand darf, entscheidet polkit.** Administrator ist, wer in der Gruppe `wheel`
  ist (beize-setup trägt den Besitzer dort schon ein). Beize liefert dafür eine Regel
  in `/usr/share/polkit-1/rules.d`, wie Fedora und Arch. Gefragt wird nach dem
  **eigenen** Passwort; die Vorgaben von systemd und polkit (z. B. wie lange es gemerkt
  wird) bleiben unverändert.
- **polkit 127** mit duktape als JavaScript-Engine (upstream seit 126 die einzige;
  mozjs entfällt). Die Passwortprüfung läuft über den socket-aktivierten
  `polkit-agent-helper@.service` als eigener Dienst (seit 127 ohne setuid). Auf der
  Konsole fragt `pkttyagent`, das run0 selbst startet.
- **pkexec wird nicht ausgeliefert** (setuid-root by design, PwnKit). Wer Rechte
  braucht, nimmt run0.
- **root bekommt kein Passwort mehr** (`!` in `/etc/shadow`), wie bei macOS und bei
  Fedora/Ubuntu mit Administrator-Konto. Anmelden als root ist damit nicht möglich;
  die Notfall-Shell hängt am Recovery-Key (ADR 0016), nicht an root. beize-setup
  kopiert den Hash nicht mehr.
- **Wiederherstellung auch bei gelungenem Start** (Voraussetzung für das Sperren von
  root): Bisher erreicht man das Notfall-Tor nur, wenn der Start scheitert. Wer sein
  Passwort vergisst oder sich aus `wheel` entfernt, käme nicht mehr hinein. Das UKI
  bekommt deshalb ein zweites, mitsigniertes Profil „Wiederherstellung“ (UKI-Profile,
  systemd ≥ 257), das im Bootmenü erscheint und direkt zum Notfall-Tor führt: Zugang
  nur mit dem Recovery-Key, wie die macOS-Wiederherstellung. Die PCR-11-Richtlinie
  gibt den TPM-Schlüssel für dieses Profil nicht frei.
- **Kein setuid, kein setgid, keine Datei-Capabilities – geprüft und erzwungen:**
  - `bz` bricht beim Bau ab, wenn eine Datei in `/usr` oder der initrd ein solches Bit
    oder ein `security.capability`-Attribut hat (wie die ELF-Prüfung, ADR 0014).
  - `/usr` und die Root-Partition werden mit `nosuid,nodev` eingehängt
    (`mount.usrflags=`, `rootflags=` in der signierten Kommandozeile). Auch was später
    jemand nach `/etc`, `/var` oder `/home` legt, bekommt so keine Rechte. Vorbild:
    Android seit 4.3 (keine setuid-Programme, `/system` mit `nosuid`), ChromeOS.
  - Altlasten fliegen aus den Rezepten: `dbus-daemon-launch-helper` (systemd startet
    die Busdienste), `unix_chkpwd`.
- **Programme, die klassisch Sonderrechte brauchen, bekommen den Weg ohne:**
  - `ping` und `tracepath` (iputils) über ICMP-Datagramm-Sockets; systemd erlaubt sie
    allen Gruppen (`net.ipv4.ping_group_range`, `50-default.conf`). `arping` und
    `clockdiff` brauchen rohe Sockets und kommen nicht ins Image. (Phase 5c)
  - Neue Programme mit setuid- oder Capability-Bedarf werden nicht mit Ausnahme
    aufgenommen, sondern über einen Dienst gelöst (Muster: run0, polkit-Helfer).
- **Quellen:** polkit und duktape signieren weder Archive noch Tags, glib
  veröffentlicht nur Prüfsummen. Gepinnt auf die Commits der Release-Tags bzw. per
  SHA-256, als begründete Ausnahme (`unsigned = …`, ADR 0005).
- **Prüfungen im Smoke-Test:**
  - Der Besitzer bekommt mit `run0 id -u` und seinem Passwort `0`; falsches Passwort
    und ein Benutzer außerhalb von `wheel` werden abgelehnt.
  - root kann sich an der Konsole nicht anmelden; das Profil „Wiederherstellung“
    öffnet das Notfall-Tor, nur mit dem Recovery-Key, und das TPM entsperrt dort nicht.
  - `/usr` und `/` sind `nosuid,nodev`; Gegenprobe: eine Kopie von `id` mit setuid-Bit
    bzw. Datei-Capability in `/var` läuft ohne zusätzliche Rechte.
  - `nosuid` schaltet auch Datei-Capabilities ab. Start- und Wiederherstellungspfade
    nutzen keine (Rechte vergibt systemd je Dienst), und alle Smoke-Szenarien – erster
    Start, Recovery-Key auf der Tastatur, Notfall-Tor, Neubinden – laufen mit den neuen
    Einhängeoptionen, weil sie in der signierten Kommandozeile stehen.
  - Der Besitzer pingt ohne Rechte (`ping -c1 ::1`, `127.0.0.1`). (Phase 5c)

## Konsequenzen

- Neue Pakete: polkit, duktape, glib mit pcre2, libffi und zlib. glib ist die größte
  Last (einige MB, gemessen wird beim Bau); sie kommt mit grafischen Anwendungen (GTK)
  ohnehin.
- duktape hat seit 2.7.0 (2022) kein Release mehr, das Repository wird aber gepflegt.
  Fällt es weg, betrifft das polkit upstream genauso; die Regeln von Beize bleiben
  schlicht (ECMAScript 5).
- Ohne setuid kann ein normales Programm das Passwort des Besitzers nicht selbst prüfen
  (`unix_chkpwd` fehlt). Eine Bildschirmsperre (M3+) braucht dafür einen Dienst.
- Software, die auf setuid baut (sudo-Skripte, `newuidmap` für rootless Podman), läuft
  nicht. Für Container gibt es den Weg über systemd (nsresourced, mountfsd), sobald er
  gebraucht wird.
- Passwörter ändern: ADR 0019 (Konten), noch vor M2.
