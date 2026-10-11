# 0019 – Konten: Beizes Kontodienst

Status: angenommen · 2026-10-11

## Kontext

beize-setup schrieb das Konto des Besitzers selbst in `/etc/passwd`, `group` und `shadow`.
Ein Passwort ändern oder ein vergessenes zurücksetzen konnte niemand – root ist gesperrt
(ADR 0018). Später brauchen Einstellungen (Name, Bild, Passwort), Anmeldung und
Sperrbildschirm dieselben Vorgänge. Lesen und Anmelden sind schon neutral: Programme fragen
Konten über NSS/userdb ab, Passwörter prüft PAM. Offen waren die **Schreibvorgänge** – und
wer über sie entscheidet.

Erst gebaut und wieder verworfen: systemd-homed hinter einer Beize-Bibliothek. homed will
selbst die Instanz sein: Es ersetzt ein Passwort nur mit dem alten oder einem im Konto
hinterlegten Recovery-Key, auch für root und Administratoren, und verlangte den Klartext
jedes Recovery-Keys sogar beim normalen Passwortwechsel (systemd#25876, seit 2022 offen) –
nur mit einem eigenen Patch zu umgehen. Sein Mehrwert (Verschlüsselung je Benutzer,
mitnehmbare Homes) bringt Beize nichts: Die ganze Root-Partition ist verschlüsselt (ADR 0009),
wie FileVault. Dazu Ballast: ein ständig laufender Dienst, `homectl` an Beize vorbei,
`/home/<name>.homedir`, UIDs ab 60001, keine Anmeldung ohne Passwort.

## Entscheidung

- **Beize ist die Instanz: `beize-accountsd`** (`os/beize-accounts`), wie `opendirectoryd` auf
  dem Mac. Nur er schreibt Konten; Setup, `beize-account` und später die Einstellungen sind
  seine Clients. Schnittstelle `io.beize.Accounts` über Varlink (systemds eigenes IPC: JSON
  mit NUL-Ende über einen Unix-Socket): `Create`, `ChangePassword`, `ResetPassword`, `Remove`;
  Name und Bild kommen mit den Einstellungen (M3).
- **Gespeichert als systemd-Benutzerdatensätze in `/etc/userdb`** (`<name>.user`, der Hash in
  `<name>.user-privileged` nur für root, eigene Gruppe, `<name>:wheel.membership` für
  Administratoren, Links je ID) – systemds Standardformat, wie macOS lokale Konten als Dateien
  in `/var/db/dslocal` hält. nss-systemd gibt sie an alle Programme und an PAM; Anmeldung,
  run0, polkit und `wheel` laufen unverändert über pam_unix (Prototyp und Smoke-Test). Konten
  ohne Datei dort (root, Systembenutzer in `/etc/passwd`) fasst der Dienst nicht an.
- **Rechte durch Beweis, nicht durch Nachfrage** (der Kernel nennt den Aufrufer,
  `SO_PEERCRED`):
  - eigenes Passwort ändern – mit dem aktuellen;
  - Konto anlegen, Passwort eines anderen zurücksetzen – ein Administrator mit seinem eigenen
    Passwort, wie auf dem Mac;
  - root darf alles ohne Passwort; root erreicht man nur mit dem Passwort eines Administrators
    (run0) oder dem Recovery-Key (Wiederherstellung). Konto entfernen: nur root.
  - Ein falscher Beweis kostet 2 s wie an der Anmeldung (`pam_faildelay`): Der Dienst ist
    kein schnellerer Weg zum Raten.
- **Ein Recovery-Key für alles, ohne ihn weiterzureichen:** Vergessenes Passwort →
  Wiederherstellung (ADR 0018) mit dem Recovery-Key → Root-Shell → `beize-account reset
  <name>`. Der Key steckt in keinem Konto.
- **Abgeschottet und ohne Dauerläufer:** `beize-accounts.socket` startet je Verbindung einen
  eigenen Prozess (Accept=yes, wie systemds userdb-Dienst und polkits Passwort-Helfer). Er darf
  nur `/etc/userdb` und `/home` schreiben, hat kein Netz, nur `CAP_CHOWN`, `CAP_FOWNER`,
  `CAP_DAC_OVERRIDE` und systemds Systemaufruf-Filter. Der Socket gehört auch zur
  Wiederherstellung (`rescue.target`).
- **Ohne neue Abhängigkeiten:** JSON liest libsystemd (sd-json), Passwörter hasht und prüft
  libxcrypt (yescrypt, Vergleich in konstanter Zeit); Geheimnisse werden nach Gebrauch
  genullt.
- **Regeln an einer Stelle:** Namen (klein, portabel, keine Systemnamen) und Passwörter
  (mindestens 8 Zeichen) prüft die Bibliothek, für Clients und Dienst gleich.

## Konsequenzen

- systemd ohne homed; Personen bekommen UIDs ab 1000, das Home ist `/home/<name>`.
- Das Konto des Besitzers ist Administrator; `beize-account add [--administrator] NAME` legt
  weitere an (bis M3 auf der Konsole).
- Verschlüsselung je Benutzer gibt es nicht – bewusst: eine für die ganze Platte, an TPM und
  Secure Boot gebunden. Wäre sie je nötig, wäre sie ein Modul hinter derselben Schnittstelle.
- Der Dienst ist eigener Code mit Rechten: klein, abgeschottet, jede Regel mit Gegenprobe
  getestet (Unit-Tests der Regeln, Smoke-Test im echten System).
- Bestehende Installationen: neu installieren (ohne Updater ohnehin der Weg, Phase 6).

## Abnahme

Smoke-Test: Setup legt das Konto über den Dienst an; falsches aktuelles Passwort ändert
nichts, das richtige schon, run0 nimmt das neue; ein Administrator legt `guest` an (nicht mit
falschem eigenen Passwort) und setzt dessen Passwort zurück, PAM nimmt es; root setzt in der
Wiederherstellung ein vergessenes Passwort; run0 lehnt ein falsches ab. Gemessen in der VM:
System-Phase unverändert (1,63 s), kein Dienst im Leerlauf, run0 0,59 s, Passwort ändern
~0,1 s.
