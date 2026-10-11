# 0018 – Rechte: run0 + polkit, kein setuid, Wiederherstellung

Status: angenommen · 2026-10-11

## Kontext

Beize hatte keinen Weg, als Besitzer Verwaltungsaufgaben zu erledigen: root meldete
sich an der Konsole mit demselben Passwort an wie der Besitzer (beize-setup kopierte
den Hash), `run0` lag im Image, konnte ohne polkit aber nur root selbst Rechte geben.

Der klassische Weg ist `sudo`: ein setuid-root-Programm, das in der Umgebung des
Aufrufers startet (Umgebungsvariablen, offene Dateien, Terminal, Ressourcenlimits) und
sich erst danach selbst einschränkt. Jede Lücke darin ist eine Rechteausweitung; sudo
und pkexec hatten genau solche (CVE-2021-3156 „Baron Samedit“, CVE-2021-4034 „PwnKit“).
`sudo-rs` und `doas` ändern die Sprache bzw. den Umfang, nicht das Modell.

In `/usr` war keine Datei setuid, setgid oder mit Datei-Capabilities – Zufall der
Rezepte (`--disable-makeinstall-setuid`, Installation ohne root), nicht abgesichert.

## Entscheidung

- **Rechte über `run0`, nicht über sudo.** run0 bittet den Dienstmanager, den Befehl
  als root zu starten: ein frischer Dienst mit sauberer Umgebung und eigenem Terminal,
  im Journal sichtbar, ohne Programm mit Sonderrechten auf der Seite des Aufrufers.
  systemd bringt run0 seit Version 256 genau als Ersatz für sudo mit.
- **Ob jemand darf, entscheidet polkit 127** (duktape als JavaScript-Engine, seit 126
  upstream die einzige). Administrator ist, wer in `wheel` ist – upstreams eigene
  `50-default.rules`, keine Beize-Regel. Gefragt wird nach dem **eigenen** Passwort;
  andere Benutzer bräuchten das eines Administrators. Die Vorgaben von systemd und
  polkit (z. B. wie lange eine Anmeldung gilt) bleiben unverändert. systemd wird mit
  `-Dpolkit=enabled` gebaut; ohne die Option lehnt es jeden außer root ab, ohne polkit
  zu fragen.
- **Passwörter prüft ein abgeschotteter Dienst**, nicht ein setuid-Programm: der
  socket-aktivierte `polkit-agent-helper@.service` (seit 127). Auf der Konsole fragt
  `pkttyagent`, das run0 selbst startet. Ein Drop-in schickt die Fehlermeldungen des
  Helfers ins Journal statt in den Socket zum Agenten – upstreams eigene Korrektur nach
  127 (3d07db95a4); mit polkit 128 entfällt es.
- **pkexec wird nicht ausgeliefert** (setuid-root by design, PwnKit).
- **root hat kein Passwort** (`!*`), wie das deaktivierte root von macOS und root bei
  Fedora mit Administrator-Konto. beize-setup sperrt root ausdrücklich, auch wenn es
  vorher ein Passwort hatte. Anmelden als root geht nicht; root gibt es über run0 oder
  über den Recovery-Key.
- **Wiederherstellung auch bei gelungenem Start**, Voraussetzung für das Sperren von
  root: Wer sein Passwort vergisst oder sich aus `wheel` entfernt, braucht einen Weg
  hinein, der nicht am Konto hängt – wie die macOS-Wiederherstellung.
  - Das UKI hat ein zweites Profil, „Recovery“ im Menü von systemd-boot, mit
    `systemd.unit=rescue.target beize.recovery`. Nur Beizes eigenes Profil hat eine
    signierte PCR-11-Richtlinie (`--sign-profile`); für die Wiederherstellung gibt
    das TPM nichts frei, die initrd fragt nach dem Recovery-Key.
  - Das Notfall-Tor (ADR 0016) übernimmt diesen Key aus dem Schlüsselbund-Cache von
    systemd-cryptsetup, prüft ihn gegen den LUKS2-Header und öffnet eine Root-Shell mit
    den Daten, ohne ein zweites Mal zu fragen.
  - PCR 12 kennt den Wiederherstellungs-Start genau: systemd-boot misst die Ladeoptionen
    `@1`, systemd-stub die Profilnummer `1`. `beize-tpm check-boot` lässt nur diesen
    Wert neben dem unveränderten Start durch; `reseal` bindet das TPM nie daran.
  - NvPCR-Einrichtung und -Messungen entfallen in der Wiederherstellung: Sie brauchen
    die signierte initrd-Richtlinie, die dieses Profil absichtlich nicht hat.
  - Erreichbar beim Start über das Menü von systemd-boot (Taste gedrückt halten);
    das Zurücksetzen des Passworts dort bringt ADR 0019.
- **Keine Datei verleiht Rechte – geprüft und erzwungen:**
  - `bz` bricht ab, wenn eine Datei in `/usr` oder der initrd setuid oder setgid ist.
  - Datei-Capabilities sind ausgeschlossen, weil kein Image erweiterte Attribute trägt:
    `/usr` wird mit `mkfs.erofs -x-1` geschrieben, die initrd ist ein cpio-Archiv.
  - `/usr` und die Root-Partition werden mit `nosuid,nodev` eingehängt
    (`mount.usrflags=`, `rootflags=` in der signierten Kommandozeile): Auch was später
    in `/etc`, `/var` oder `/home` liegt, bekommt keine Rechte – auch keine
    Capabilities. Vorbild: Android seit 4.3 (keine setuid-Programme, `/system` mit
    `nosuid`), ChromeOS.
  - `dbus-daemon-launch-helper` fliegt aus dem Rezept: systemd startet alle
    Busdienste (`SystemdService=`). `unix_chkpwd` bleibt, ohne setuid-Bit: pam_unix
    liest `/etc/shadow` seit Linux-PAM 1.6 nie selbst, sondern immer über dieses
    Programm; ohne das Bit funktioniert es nur für Aufrufer, die die Datei ohnehin
    lesen dürfen.
- **Programme, die klassisch Sonderrechte brauchen, bekommen den Weg ohne:** `ping` und
  `tracepath` über ICMP-Datagramm-Sockets (`net.ipv4.ping_group_range`, systemds
  `50-default.conf`; Phase 5c), `arping` und `clockdiff` nicht. Neue Fälle werden über
  einen Dienst gelöst (Muster: run0, polkit-Helfer), nicht über eine Ausnahme.
- **Quellen:** glib (signierter Tag, Submodul gvdb über den signierten Baum), pcre2 und
  zlib signiert; polkit, duktape und libffi signieren nichts – gepinnt, als begründete
  Ausnahme (ADR 0005).
- **Prüfungen im Smoke-Test:**
  - Als Besitzer an der Konsole: run0 lehnt ein falsches Passwort ab (ohne es
    anzuzeigen) und läuft mit dem eigenen als root; polkit fragt nach dem Passwort
    des Besitzers; root ist gesperrt.
  - `/` und `/usr` sind `nosuid,nodev`; Gegenprobe: eine Kopie von `id` mit setuid-Bit
    und eine von `cat` mit `cap_dac_read_search` in `/var` bringen nichts.
  - Die Wiederherstellung (per `LoaderEntryOneShot` gewählt) fragt einmal nach dem
    Recovery-Key, öffnet eine Root-Shell mit den Daten, keine Unit scheitert; der
    nächste normale Start entsperrt wieder per TPM.
  - Alle übrigen Szenarien – erster Start, Recovery-Key auf der Tastatur, Notfall-Tor,
    Neubinden – laufen mit den neuen Einhängeoptionen.

## Konsequenzen

- Neue Pakete: polkit, duktape, glib mit pcre2 (ohne JIT), libffi und zlib, zusammen
  6 MB in `/usr`. glib kommt mit grafischen Anwendungen (GTK) ohnehin.
- duktape hat seit 2.7.0 (2022) kein Release mehr, das Repository wird gepflegt. Fällt
  es weg, betrifft das polkit upstream genauso; Beize hat keine eigenen Regeln.
- Eine Bildschirmsperre (M3+) prüft Passwörter über einen Dienst, nicht selbst.
- Software, die auf setuid baut (`newuidmap` für rootless Podman), läuft nicht; für
  Container gibt es den Weg über systemd (nsresourced, mountfsd).
- `bz` kann Git-Quellen mit Submodulen laden (`submodules = true`), jedes genau mit
  dem Commit, den der signierte Baum nennt.
