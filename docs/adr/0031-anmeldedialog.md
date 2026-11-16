# 0031 – Anmeldedialog: die Shell als polkit-Agent

Status: Angenommen · 2026-11-15 (Besitzer, am Mock-up)

## Kontext

polkit fragt nach dem Passwort nur über einen **Agenten**, der für die Sitzung angemeldet
ist. Beize hat auf dem Desktop keinen. Es gibt nur `pkttyagent`, das run0 im Terminal
selbst startet (ADR 0018). Deshalb wird jede Aktion, die eine grafische App anfragt und die
eine Anmeldung braucht, ohne Frage abgelehnt. Betroffen sind zum Beispiel:
- Uhrzeit oder Zeitzone stellen (timedated),
- Neustart, während jemand anderes angemeldet ist (logind),
- später die Einstellungen,
- die Systemauthentifizierung von 1Password.

Wie andere Desktops das lösen:
- **GNOME Shell** ist selbst der Agent (`Shell.PolkitAuthenticationAgent`, über
  libpolkit-agent-1). Der Dialog ist systemmodal und enthält den Namen des Benutzers, ein
  Passwortfeld sowie „Cancel“ und „Authenticate“. Bei einem Fehler zeigt er „Sorry, that
  didn't work. Please try again.“
- **KDE** nutzt einen eigenen Prozess (polkit-kde-agent-1). **COSMIC** baut den Agenten in
  Rust in cosmic-osd ein und spricht den Helfer direkt an.
- **macOS** zeigt das Autorisierungsfenster: „*App* wants to make changes.“, darunter Name und
  Passwort, dann „Cancel“ und „OK“. Bei einem falschen Passwort schüttelt es sich und leert
  das Feld.

Für den Agenten sind zwei Wege belegt, beide am Quelltext von polkit 127/128 (das Protokoll
ist zwischen den Versionen gleich):
- **Agent:** Der Agent meldet sich bei `org.freedesktop.PolicyKit1.Authority` mit
  `RegisterAuthenticationAgent(subject, locale, object_path)` für eine Sitzung an. polkitd ruft
  dann `BeginAuthentication` auf, mit der Aktion, der Meldung, den Details, einem Cookie und den
  Identitäten, die die Aktion erlauben dürfen. Zum Abbruch ruft polkitd `CancelAuthentication`.
  Die Details enthalten `polkit.subject-pid` und `polkit.caller-pid`.
- **Helfer:** Der Agent verbindet sich mit `/run/polkit/agent-helper.socket` und schickt eine
  Zeile mit dem Benutzernamen und eine mit dem Cookie. Der Helfer antwortet zeilenweise mit
  `PAM_PROMPT_ECHO_OFF …`, `PAM_PROMPT_ECHO_ON …`, `PAM_ERROR_MSG …` und `PAM_TEXT_INFO …`. Auf
  eine Frage antwortet der Agent mit einer Zeile. Am Ende sendet der Helfer `SUCCESS` oder
  `FAILURE`. Den Aufrufer erkennt der Helfer selbst über `SO_PEERCRED`/`SO_PEERPIDFD`; das
  Ergebnis meldet er polkitd direkt, nicht über den Agenten.

run0 startet `pkttyagent` mit `--fallback` (systemd 262, `src/shared/polkit-agent.c`).
polkitd nimmt einen solchen Agenten nur, wenn die Sitzung keinen eigenen hat
(`polkitbackendinteractiveauthority.c`). Mit einem Agenten der Shell fragt also auch run0 im
Terminal über den Dialog, genau wie unter GNOME und KDE.

## Entscheidung

- **Die Shell ist der Agent,** wie bei GNOME Shell und COSMIC. Ein eigener Prozess hätte keinen
  Grund: Den Dialog, das Overlay und die Tastatur hat die Shell schon (ADR 0028 §3, ADR 0030).
  Sie meldet sich beim Start für ihre Sitzung an, also die Display-Sitzung des Benutzers (wie
  bei `may()`, ADR 0028), über die eigene sd-bus-Anbindung (`beize-systemd`). Dazu kommt dort
  ein Objekt, das Aufrufe annimmt. Die Shell läuft außerhalb jeder Sitzung, deshalb nimmt
  polkit für sie `sd_uid_get_display` (`polkitbackendsessionmonitor-systemd.c`). Nur für diese
  Sitzung darf sie sich anmelden, und für sie fragen auch die Apps, die ebenfalls im
  Service-Manager laufen. GNOME Shell im systemd-Modus macht es ebenso. GDM legt ihre Sitzung
  aber gleich als Wayland an, Beizes tty1 wird es erst mit `SetType`. logind wählt die
  Display-Sitzung dabei nicht neu, also bliebe eine frühere Anmeldung (serielle Konsole, SSH)
  Display, und der Agent würde für deren `run0` fragen. Das behebt der eigene Patch in
  `recipes/patches/systemd` (gesendet vom Besitzer). Bis Upstream ihn hat, trägt Beize ihn wie
  die Kernel-Patches.
- **Den Helfer spricht sie direkt über den Socket an,** ohne libpolkit-agent-1 (GObject).
  Einen setuid-Helfer als Ausweichweg gibt es nicht (ADR 0018). Fehlt der Socket, wird
  abgelehnt und ins Journal geschrieben.
- **Der Dialog** ist `beize_ui::dialog` mit einem Passwortfeld zwischen Text und Knöpfen, in
  der Breite der Knöpfe, auf einem Overlay mit Tastatur. Solange er offen ist, hält Beize die
  Tastatur: Ein neues Fenster öffnet sich dahinter und kann kein Tippen abfangen
  (ADR 0030). Ein Klick auf ein Fenster nimmt ihm die Tastatur, wie beim Bestätigungsdialog
  (ADR 0028 §3).
  Aufbau wie macOS 26, alles linksbündig (Besitzer, 15.11., Entwurf A am Mock-up; Bilder
  von macOS 15 und 26 zum Vergleich aus Eclectic Light, 18.12.2025):
  - **Symbol oben links:** ein Schloss in 3D, 56 px, mit dem Symbol der App als Abzeichen
    unten rechts (30 px, etwa 0,55 des Schlosses wie in macOS 26, ragt 10 px über). So zeigt
    macOS 26 seine Passwortabfrage (Bild des Besitzers, 15.11.). Das Schloss ist „Locked“ aus
    Microsofts Fluent Emoji 3D (MIT, PNG mit 256 px, als Rezept mit SHA-256 festgelegt wie
    Phosphor, der leere Rand der Emoji-Fläche abgeschnitten). Ein flaches Phosphor-Schloss,
    ob im Squircle oder freistehend, lehnte der Besitzer ab („hässlich“). Fluent nahm er als
    „gut, nicht perfekt“: Ein eigener Satz System-Symbole in dieser Art kommt später. Die
    App ergibt sich aus der Unit von `polkit.subject-pid` (`app-…`, Symbol und Name wie im
    Dock). Ist keine zu finden, steht das Schloss allein. Das Bild wird einmal als Textur
    geladen, wenn der Dialog aufgeht, und kostet beim Zeichnen so viel wie ein Buchstabe.
  - **Titel:** der Name der App, fett, 12 px unter dem Symbol. Ohne erkennbare App
    „Authentication Required“ (GNOME), und die Meldung steht unverändert da.
  - **Was sie will:** „*App* wants to *…*.“, gebildet aus der Meldung der Aktion von polkit:
    „Authentication is required to …“ und „… required for an application to …“ werden zu
    „*App* wants to …“. Das gilt für 133 der 134 Aktionen in Beize. Eine andere Meldung
    steht unverändert da. Englisch wie bisher (ADR 0017). Wie genau der Satz ist, legt der
    Dienst fest: systemd setzt die Unit ein („Terminal wants to restart 'sshd.service'.“, aus
    `polkit_message_for_job`), eine eigene Aktion von Sift könnte den Ordner nennen.
  - **Was zu tun ist:** „Enter your password to allow this.“ (macOS), 8 px unter dem Absatz
    davor.
  - **Feld:** Platzhalter „Password“, Punkte statt Zeichen, Beizes blauer Umriss, solange es
    die Tastatur hat. Rechts im Feld steht das Caps-Lock-Zeichen, nur solange Caps Lock an ist
    (macOS). Es ist kein Knopf. Ein Auge zum Aufdecken gibt es nicht (macOS). Einfügen geht,
    Kopieren nicht (macOS). Fragt PAM mit `ECHO_ON`, steht der Text offen da. Geschätzt:
    35 hoch wie die Knöpfe, Ecken 10, 16 unter dem Text.
  - **Knöpfe nebeneinander:** „Cancel“ links, „Allow“ rechts, je halb so breit, 8 Abstand
    (Besitzer, 15.11.). „Allow“ ist das Verb aus dem Satz darüber, wie die HIG Knöpfe
    benennt. Return ist „Allow“, Escape ist „Cancel“, Pfeile und Tab wie beim
    Bestätigungsdialog.
  - **Falsches Passwort:** Das Feld wird leer und behält die Tastatur. Darunter steht rot
    (13 px, Apples Systemrot) „Sorry, that didn't work. Please try again.“ (GNOME). Das
    Schütteln von macOS kommt mit den Federanimationen (Shell g). Während PAM prüft, nimmt
    das Feld nichts an.
  - `PAM_ERROR_MSG` und `PAM_TEXT_INFO` stehen an derselben Stelle wie die Fehlerzeile.
  - **Auch der Bestätigungsdialog** (Neustart, Ausschalten, Abmelden) bekommt die Knöpfe
    nebeneinander, „Cancel“ links, die Aktion rechts: in Bestätigungsdialogen immer
    (Besitzer, 15.11., am Mock-up: 148 statt 191 px hoch). Ob Auswahldialoge mit mehreren
    Möglichkeiten sie untereinander stapeln, wird besprochen, wenn es einen gibt. Damit
    ändert sich ADR 0028 §3 an dieser Stelle.
- **Identität:** der Benutzer selbst, wenn polkit ihn unter den Identitäten nennt. Das ist
  der Normalfall, weil der Besitzer in `wheel` ist. Sonst wird zwischen mehreren über ein Menü
  gewählt (GNOME, `beize_ui::menu`), eine einzelne wird genannt.
- **Eine Anfrage nach der anderen:** Weitere warten, bis die erste beantwortet ist (GNOME).
  `CancelAuthentication` schließt den Dialog.
- **Das Passwort** liegt nur in einem Puffer, der nach dem Senden überschrieben wird. Es
  steht in keinem Log, wird nur an den Helfer geschickt und bleibt nicht im Feld zurück.
- **polkit 128** (15.11.): die beiden Lücken im Helfer (CVE-2026-4897, CVE-2026-85498) sind
  geschlossen. Das war die Voraussetzung, dass die Shell mit dem Helfer spricht.

## Prüfungen (Smoke-Test, Desktop)

- Der Agent gehört zur Display-Sitzung, und die ist tty1, obwohl sich die serielle Konsole
  vorher angemeldet hat (logind-Patch). Deren run0 fragt weiter auf der Konsole.
- `run0 true` im Terminal des Desktops öffnet den Dialog (Overlay `authentication`). Mit dem
  Passwort über QMP-Tasten endet run0 mit 0. Mit einem falschen Passwort bleibt der Dialog
  offen, und die Fehlerzeile steht da. Danach muss allein das richtige Passwort reichen, das
  Feld war also leer. Escape lässt run0 ablehnen.
- Der Text: Die Shell meldet Titel, Sätze und Abzeichen ins Journal (ohne Passwort). Für run0
  lauten sie „Terminal“ und systemds Satz für den Job, „Terminal wants to start transient
  unit 'run-….service'.“; das Abzeichen ist das eigene Symbol des Terminals.
- Ein neues Fenster während des Dialogs öffnet sich dahinter, ohne Tastatur (ADR 0030).
- Keines der beiden Passwörter steht in einem Journal des Starts, auch nicht im System-Journal
  mit dem Helfer.
- Gegenproben: Das richtige Passwort mit einem Cookie, das polkitd nie vergab, bekommt vom
  Helfer `FAILURE`. Der Benutzer selbst darf den Agenten nicht aufrufen (`busctl`: Access
  denied), nur polkitd. Ohne angemeldeten Agenten wird der Test rot (am 16.11. so gesehen).

## Konsequenzen

- run0 im Terminal des Desktops fragt im Dialog, nicht mehr im Terminal (polkits Entwurf, wie
  GNOME und KDE). Auf der Konsole ohne Desktop bleibt `pkttyagent`.
- Grafische Apps, die polkit nutzen, funktionieren. Für 1Password braucht es zusätzlich einen
  Weg, Apps zu installieren (STATUS).
- Neu in beize-ui ist ein Textfeld (Passwortmodus zuerst). Es wird der Baustein für
  Einstellungen und Sift.
- Schütteln, eine Auswahl der Identität mit Bild und Touch-ID-artige Wege (fprintd) kommen
  später.
