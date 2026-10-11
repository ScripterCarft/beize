# 0016 – initrd aus Funktionen, Notfall-Shell, hwdb im Image

Status: angenommen · 2026-10-10

## Kontext

Die initrd entsteht aus ganzen Paketen (systemd, util-linux, e2fsprogs, bash, kmod,
beize-tpm), nur Locale-Daten und Bootloader fehlen. Ergebnis: 78 MB entpackt,
19,3 MiB komprimiert, 154 Programme, 347 Units. Auf der Hardware laufen davon gut
20 Units (udev, TPM-Messungen, verity, repart, cryptsetup, Beizes eigene Prüfungen).
Der Rest ist Ballast: Er wird bei jedem Start mitgeladen und entpackt (das UKI ist
37 MB groß), vergrößert die Angriffsfläche vor dem Entsperren und startet teils sogar
Arbeit, die dort nichts zu suchen hat (`ldconfig.service` lief auf der Hardware in der
initrd). Mit jedem Paket, das wächst, wächst die initrd mit, ohne dass jemand es
entschieden hat.

Die hwdb (udevs Datenbank für Gerätekorrekturen: Tastaturbelegungen, Sensoren,
Mäuse) liegt nur als Quelltext in `/usr/lib/udev/hwdb.d`; ohne die kompilierte
`hwdb.bin` wendet udev keine dieser Korrekturen an.

## Entscheidung

- **Die initrd wird aus Funktionen zusammengesetzt, nicht aus Paketen.**
  `images/initrd.toml` nennt die Funktionen, jede mit Begründung: Grundsystem
  (systemd als PID 1, journald, udev), Plattencontroller laden, `/usr` finden und mit
  verity öffnen, Root beim ersten Start anlegen (repart, mkfs.ext4), Root entsperren
  (cryptsetup, TPM, Recovery-Key-Eingabe), TPM-Messungen, Beizes eigene Prüfungen,
  Notfall-Shell. Eine Funktion besteht aus Units (samt Drop-ins), zusätzlichen
  Programmen und udev-Regeln und den dlopen-Funktionen, die sie braucht.
- **Den Rest leitet `bz` ab:** Programme aus den `Exec*=`-Zeilen der Units und den
  Generatoren, Bibliotheken aus den ELF-Abhängigkeiten (`DT_NEEDED`) und den
  dlopen-Metadaten (`.note.dlopen`, Spezifikation der UAPI-Gruppe; unser systemd
  liefert sie). Eine per dlopen geladene Bibliothek kommt nur hinein, wenn eine
  Funktion das Feature nennt (z. B. `cryptsetup`, `fdisk`, `tpm2`). `bz` bricht ab,
  wenn eine Unit ein fehlendes Programm aufruft, ein genanntes Feature in keiner
  Notiz vorkommt oder eine Bibliothek nicht auflösbar ist.
- **Vorbild:** dracut (Fedora, RHEL, openSUSE) setzt die initrd ebenfalls aus Modulen
  mit Programmlisten zusammen und löst Bibliotheken über die ELF-Abhängigkeiten auf.
  Beize macht dasselbe deklarativ, ohne Shell-Module, und nutzt die dlopen-Metadaten
  statt Handlisten. mkosi-initrd (ganze Pakete, Ausschlusslisten) ist unser
  bisheriger Stand und der Grund für den Ballast.
- **Notfall-Shell nur mit Recovery-Key.** Scheitert der Start, zeigt die Konsole den
  Grund (fehlgeschlagene Units, letzte Fehler im Journal) und bietet an: Neustart,
  Ausschalten oder Notfall-Shell. Die Shell gibt es erst, wenn der Recovery-Key gegen
  den LUKS-Header der Root-Partition geprüft ist (libcryptsetup im Prüfmodus, ohne
  etwas zu öffnen). Ein falscher Key, Ctrl+C oder ein Absturz der Abfrage führen nie in
  eine Shell, sondern zurück zur Auswahl. Eine neue Version auf Probe wartet hier nicht,
  sondern startet neu, bis systemd-boot die vorige wählt (ADR 0020).
  - **Warum der Recovery-Key:** Er ist das einzige Merkmal, das der Besitzer immer hat
    und das auch dann gilt, wenn das System nicht startet (das Benutzerpasswort liegt
    in der verschlüsselten Root-Partition). Mit ihm käme er ohnehin an alle Daten; die
    Shell gibt ihm also nichts, was er nicht schon hat. Vorbild: Windows verlangt in
    WinRE den BitLocker-Wiederherstellungsschlüssel, macOS in recoveryOS die
    Anmeldung, bevor es an die Daten geht. Nicht übernommen: Fedora kopiert den
    root-Passwort-Hash des Bau-Rechners in die initrd, das geht bei einem zentral
    gebauten Image nicht; ohne Hash verweigert `sulogin` jeden Zugang.
  - **Warum die Prüfung auch bei schon entsperrter Platte:** Das TPM entsperrt ohne
    Zutun des Benutzers. Ließe sich ein Fehler nach dem Entsperren erzwingen (z. B. ein
    beschädigter `/usr`-Slot), hätte ein Dieb sonst root-Zugriff auf entsperrte Daten.
    Genau diese Lücke hatte Debian/Ubuntu (CVE-2016-4484: Shell nach Fehlversuchen).
  - **Vor der Shell wird PCR 11 mit „emergency“ erweitert:** Keine der signierten
    TPM-Richtlinien passt danach mehr, aus der Shell heraus entsperrt das TPM nichts
    mehr; der nächste Start beginnt wieder sauber.
  - Dasselbe Tor gilt nach dem Wechsel ins System (`emergency`/`rescue`), statt
    `sulogin`. Die Shell läuft nur von signiertem Code (UKI, `/usr` unter verity), nur
    an der lokalen Konsole; die Kernel-Kommandozeile ist im UKI signiert, `rd.shell`
    oder `systemd.debug_shell` lassen sich nicht unterschieben (`debug-shell.service`
    kommt nicht in die initrd).
  - Vor dem ersten Start gibt es noch keinen Recovery-Key und keine Daten, dann auch
    keine Shell: Abhilfe ist eine Neuinstallation.
  - Smoke prüft: absichtlich gescheiterter Start → kein Zugang mit falschem Key und
    mit Ctrl+C, Zugang mit dem richtigen Key, TPM entsperrt danach nicht mehr.
- **hwdb kompiliert im Image:** `bz` erzeugt `/usr/lib/udev/hwdb.bin` beim Bauen
  (`systemd-hwdb update --usr`, mit unserem systemd), die Quellen `hwdb.d` werden
  nicht ausgeliefert (wie image-basierte Systeme, z. B. ParticleOS; `/usr` ist
  ohnehin schreibgeschützt). In die initrd kommt die hwdb nicht: Dort braucht nur die
  Recovery-Key-Eingabe eine Tastatur, und dafür genügt der Standard.
- **Prüfungen:** Die Smoke-Szenarien (erster Start, TPM, Recovery-Key, Neubinden)
  decken jede Funktion ab und bleiben der Abnahmetest. `bz` gibt Größe und Anzahl der
  Programme der initrd aus. Das Zeitbudget der initrd-Phase in `BOOT_BUDGET` bleibt:
  In der VM bestimmt das Software-TPM die Dauer, nicht die Größe der initrd (neu
  gemessen nach dem Umbau, CI 3,5–4,2 s wie vorher).

## Konsequenzen

- Ein neues Programm in der initrd ist eine bewusste Änderung an `images/initrd.toml`,
  kein Nebeneffekt eines Paket-Updates.
- Wächst ein Paket um eine dlopen-Abhängigkeit, merkt das `bz` nicht von selbst – fehlt
  sie in der initrd, scheitert der Smoke-Test (die Funktion läuft dann nicht).
- initrd 10,7 statt 19,3 MiB komprimiert (33 statt 78 MB entpackt), 175 statt rund
  1 300 Dateien aus Paketen, 58 statt 347 Units. Den Rest machen systemd selbst und
  libcrypto aus (zusammen 16 MB) – kleiner nur ohne Funktion. Was die Startzeit
  gewinnt, zeigt die Messung auf der Hardware.
- Eigener Code für das Notfall-Tor: `beize-tpm emergency` (Rust; Abfrage über
  systemd-ask-password, Prüfung über libcryptsetup, das initrd und `/usr` ohnehin
  haben – keine neue Bibliothek, kein `cryptsetup`-Kommandozeilenwerkzeug).
  `beize-tpm` liegt dafür auch in `/usr`. Die Gegenprobe ist Teil der Abnahme:
  ohne die PCR-11-Erweiterung entsperrt das TPM aus der Shell, und der Smoke-Test
  scheitert.
- Wer den Recovery-Key verliert und das System startet nicht mehr, kommt nicht an die
  Daten – gewollt, wie bei FileVault und BitLocker.
- udev wendet Gerätekorrekturen aus der hwdb an (z. B. Tasten von Notebooks und
  Spezialtastaturen, Beschleunigungssensoren).
