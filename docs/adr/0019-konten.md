# 0019 – Konten: eine Beize-Schnittstelle, dahinter systemd-homed

Status: angenommen · 2026-10-11

## Kontext

beize-setup schrieb das Konto des Besitzers selbst in `/etc/passwd`, `group` und `shadow`.
Ein Passwort ändern oder ein vergessenes zurücksetzen konnte niemand – root ist gesperrt
(ADR 0018), die Wiederherstellung öffnete nur eine Shell. Später brauchen Einstellungen
(Name, Bild, Passwort), Anmeldung und Sperrbildschirm dieselben Vorgänge.

Lesen und Anmelden sind schon neutral: Programme fragen Konten über NSS/userdb ab,
Passwörter prüft PAM. Offen waren die **Schreibvorgänge**.

systemd-homed verwaltet Konten als signierte JSON-Datensätze (`/var/lib/systemd/home/`,
dazu eine Kopie im Home selbst), mit Polkit-Prüfung für jeden Vorgang, Kontobild und
PAM-Modul. Ein Passwort ersetzt homed nur, wenn das alte Passwort oder ein **im Konto
hinterlegter Recovery-Key** (modhex64, dasselbe Format wie der LUKS-Recovery-Key) den
bisherigen Datensatz öffnet – auch für root nicht anders, weil das Home mit dem Passwort
verschlüsselt sein kann.

## Entscheidung

- **Eine Schnittstelle für Konten: `beize-accounts`** (`os/beize-accounts`, Vorbild macOS
  Open Directory: ein API, dahinter austauschbare Module). Setup, Einstellungen und
  Wiederherstellung sprechen nur sie. Heute: anlegen, entfernen, Passwort ändern, Passwort
  mit dem Recovery-Key zurücksetzen; Name und Bild kommen mit den Einstellungen (M3). Den
  Vertrag prüft der Smoke-Test im echten System; jedes Modul muss ihn erfüllen.
- **Modul homed, Speicher „directory“.** Das Home ist ein Verzeichnis auf der
  LUKS2-Root-Partition (ADR 0009): eine Verschlüsselung für alle, wie FileVault – keine
  zweite je Benutzer. homed bindet es beim Anmelden unter `/home/<name>` ein. GNOME OS plant
  denselben Weg (gnome-build-meta #655); als Standard liefert ihn noch keine große
  Distribution aus. Die Bibliothek ruft homeds D-Bus-API über libsystemd (sd-bus, im Image
  ohnehin) als der aufrufende Benutzer auf; homed prüft Polkit selbst, Beize braucht keinen
  eigenen Dienst mit Rechten. Geheimnisse werden nach Gebrauch genullt, sd-bus löscht die
  Nachrichten (`sd_bus_message_sensitive`).
- **Ein Recovery-Key für alles.** Das Setup hinterlegt den LUKS-Recovery-Key aus
  `/run/beize/recovery-key` (nur im RAM, bis zum Neustart) als homed-Recovery-Key des Kontos.
  Er entsperrt die Daten, öffnet die Wiederherstellung und setzt ein vergessenes Passwort
  zurück, wie der FileVault-Key auf dem Mac. Für den Alltag braucht das Konto ihn nicht.
- **Beizes eigener Patch an homed** (die einzige Ausnahme von „keine eigenen Patches“,
  bewusst mit dem Besitzer): homed verlangte bei *jeder* Passwortänderung den Klartext jedes
  hinterlegten Recovery-Keys – Passwort ändern hätte das Eintippen des Recovery-Keys
  bedeutet. Nötig ist der Klartext nur, um Schlüsselplätze von LUKS und fscrypt nachzuziehen;
  `recipes/patches/systemd/0001-…` verlangt ihn nur noch dort. Die Prüfung, ob altes
  Passwort oder Key stimmt, bleibt unverändert (`home_load_embedded_identity`). Upstream seit
  2022 offen (systemd#25876, wartet auf den Umbau der Schlüsselhaltung, #31741); der Patch
  ist dort vorgeschlagen und fällt weg, sobald upstream es löst. `bz` wendet Patches aus
  `recipes/patches/<paket>/` beim Auspacken an, ihr Inhalt zählt zum Fingerabdruck.
- **Zurücksetzen in der Wiederherstellung** (ADR 0018): Recovery-Key → Notfall-Tor → Root-Shell
  → `beize-account reset <name>`. Das Tor legt den geprüften Key für die Shell nach
  `/run/beize/recovery-key` und löscht ihn vor dem Neustart; niemand tippt ihn zweimal.
- **Passwort ändern:** `beize-account passwd` mit dem alten Passwort (bis M3 auf der Konsole).
- **Anmelden:** `pam_systemd_home` in `system-auth`, wie Fedoras authselect-Profil
  „with-systemd-homed“ – aber nur bei `user_unknown` (kein homed-Konto) weiter zu pam_unix;
  jeder andere Fehlschlag endet dort (`default=die`), sonst fragte pam_unix ein zweites Mal
  nach einem Passwort, das es für homed-Konten nie prüfen kann. Nach einem falschen Passwort
  fragt pam_systemd_home bis zu fünfmal nach, eine leere Eingabe bricht ab (wie sudo).
  Administrator bleibt, wer in `wheel` ist (`memberOf`).
- **Systembenutzer bleiben in `/etc/passwd`** (systemd-sysusers), root bleibt gesperrt.
- **Bau:** systemd mit `-Dhomed=enabled -Dlibcrypt=enabled`, ohne libpwquality (keine
  Passwortregeln außer 8 Zeichen in `beize-accounts`), ohne FIDO2/PKCS#11.

## Konsequenzen

- Konten der Personen bekommen UIDs 60001–60513 (homeds fester Bereich) statt 1000.
- Ohne Anmeldung ist das Home nicht eingehängt; eine Anmeldung ohne Passwort gibt es nicht
  (homed öffnet das Home nur mit ihm, wie FileVault). Dienste des Benutzers laufen nur
  angemeldet oder mit homeds „Linger“.
- Ein später angelegtes Konto (Einstellungen, M3+) bekommt denselben Recovery-Key: Wer es
  anlegt, tippt ihn einmal ein – homed verlangt beim Anlegen den Klartext.
- **Ein Administrator setzt das Passwort eines anderen Kontos zurück** (wie auf dem Mac):
  heute nur mit dem Recovery-Key, den jedes Konto trägt. Ohne Key bräuchte es eine weitere
  Änderung an homed (Zurücksetzen durch einen Administrator bei Speicher „directory“) –
  möglich, wird mit mehreren Konten (M3+) entschieden.
- Wer den Recovery-Key hat, kann sich auch direkt mit ihm anmelden – er öffnet ohnehin die
  Daten und eine Root-Shell.
- Ein Modul `/etc/shadow` kommt erst, wenn es gebraucht wird.
- Bestehende Installationen: neu installieren (ohne Updater ohnehin der Weg, Phase 6).

## Abnahme: schnell und sparsam (erfüllt)

Gemessen in der VM gegen den Stand mit `/etc/shadow` (2026-10-11, lokal): homed im Leerlauf
2,6–3,5 MiB und 13 ms CPU seit dem Start; System-Phase 1,66–1,72 s statt 1,61 s;
Passwortprüfung von run0 0,62–0,72 s statt 0,51 s (yescrypt in homework); Passwort ändern
~0,1 s; Anmeldung getty → Shell 0,62–0,70 s (mit Passwort).
