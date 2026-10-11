# 0019 – Konten: eine Beize-Schnittstelle, dahinter systemd-homed

Status: angenommen · 2026-10-11 (unter der Abnahme unten)

## Kontext

beize-setup schreibt das Konto des Besitzers heute selbst in `/etc/passwd`, `group` und
`shadow` (`accounts.rs`). Ein Passwort ändern oder ein vergessenes zurücksetzen kann
niemand – root ist gesperrt (ADR 0018), die Wiederherstellung öffnet nur eine Shell.
Später brauchen Einstellungen (Name, Bild, Passwort), Anmeldung und Sperrbildschirm
dieselben Vorgänge.

Lesen und Anmelden sind schon neutral: Programme fragen Konten über NSS/userdb ab,
Passwörter prüft PAM. Offen sind die **Schreibvorgänge**: anlegen, Passwort ändern und
zurücksetzen, Name, Bild.

systemd-homed verwaltet Konten als signierte JSON-Datensätze (`/var/lib/systemd/home/`,
dazu eine Kopie im Home selbst), mit Polkit-Prüfung für jeden Vorgang, Kontobild
(`--avatar`) und PAM-Modul. Ein vergessenes Passwort lässt sich nur mit dem alten Passwort
oder einem **im Konto hinterlegten Recovery-Key** (modhex64, dasselbe Format wie der
LUKS-Recovery-Key) ersetzen. Ein Administrator kann es nicht einfach überschreiben, auch
root nicht (`user_record_compile_effective_passwords`, v262). So ist es gewollt, weil das
Home mit dem Passwort verschlüsselt sein kann.

## Entscheidung

- **Eine Schnittstelle für Konten, `beize-accounts`** (Rust-Bibliothek, Vorbild macOS Open
  Directory: ein API, dahinter austauschbare Module). Setup, Einstellungen und
  Wiederherstellung sprechen nur sie. Vorgänge: anlegen, Passwort ändern, Passwort mit
  dem Recovery-Key zurücksetzen, Name, Bild. Ein Vertragstest beschreibt das Verhalten und
  gilt für jedes Modul; der Smoke-Test prüft ihn im echten System.
- **Modul homed, Speicher „directory“.** Das Home ist ein Verzeichnis auf der
  LUKS2-Root-Partition (ADR 0009): eine Verschlüsselung für alle, wie FileVault auf dem
  Mac – keine zweite je Benutzer. Beim Anmelden bindet homed es unter `/home/<name>` ein,
  beim Abmelden löst es es wieder. GNOME OS plant denselben Weg (Initiative „Encrypted home
  directory“, gnome-build-meta #655); als Standard liefert ihn noch keine große Distribution
  aus – Beize ist hier früh dran, daher die Bedingung des Besitzers: nur, wenn es stabil
  läuft. homed prüft Polkit selbst; die Bibliothek ruft homeds D-Bus-API als der aufrufende
  Benutzer auf, braucht also keinen eigenen Dienst mit Rechten.
- **Ein Recovery-Key für alles.** Das Setup hinterlegt den LUKS-Recovery-Key aus
  `/run/beize/recovery-key` (nur im RAM, bis zum Neustart) als homed-Recovery-Key des
  Kontos. Derselbe Key entsperrt die Daten, öffnet die Wiederherstellung und setzt das
  Passwort zurück, wie der FileVault-Key auf dem Mac.
- **Zurücksetzen in der Wiederherstellung** (ADR 0018): Recovery-Key → Root-Shell →
  `beize-account reset <name>` fragt zweimal nach dem neuen Passwort. Der Key kommt aus dem
  Schlüsselbund der Wiederherstellung, der Besitzer tippt ihn nicht ein zweites Mal.
- **Passwort ändern im laufenden System:** `beize-account passwd` (bis M3 auf der Konsole,
  dann in den Einstellungen) mit dem alten Passwort.
- **Anmelden:** `pam_systemd_home` in `system-auth` (upstreams Beispiel), damit Konsole,
  run0/Polkit und später der Sperrbildschirm homed-Konten kennen. Administrator bleibt, wer
  in `wheel` ist (`memberOf`).
- **Systembenutzer bleiben in `/etc/passwd`** (systemd-sysusers), root bleibt gesperrt.
- **Quellen/Bau:** systemd mit `-Dhomed=enabled`, ohne libpwquality (wie heute: keine
  Regeln für Passwörter), ohne FIDO2/PKCS#11 (erst mit Sicherheitsschlüsseln).

## Konsequenzen

- Konten der Personen bekommen UIDs 60001–60513 (homeds fester Bereich) statt 1000.
- Ohne Anmeldung ist das Home nicht eingehängt: Dienste des Benutzers laufen nur
  angemeldet oder mit homeds „Linger“.
- Ein **später** angelegtes Konto (Einstellungen, M3+) bekommt denselben Recovery-Key:
  Wer es anlegt, tippt ihn dabei einmal ein – homed verlangt den Klartext jedes
  hinterlegten Keys. So gilt für jedes Konto dasselbe.
- Ein Modul `/etc/shadow` kommt erst, wenn es gebraucht wird; die Schnittstelle ist so
  geschnitten, dass es ohne Änderung der Aufrufer dazukommt.
- Wer den Recovery-Key hat, kann sich auch direkt mit ihm anmelden – er öffnet ohnehin
  schon die Daten und eine Root-Shell, die Macht ist dieselbe.
- Bestehende Installationen: neu installieren (ohne Updater ohnehin der Weg, Phase 6).

## Abnahme: schnell und sparsam

Konten sollen sich anfühlen wie auf dem Mac – sofort da, ohne Wartezeit. homed bleibt nur,
wenn es das gemessen erfüllt; sonst neu besprechen (eigenes Modul hinter derselben
Schnittstelle). Gemessen in der VM gegen den Stand mit `/etc/shadow`: Startzeit
(System-Phase), Speicher und CPU von homed im Leerlauf, Dauer von Anmeldung und
`run0`-Passwortprüfung.
