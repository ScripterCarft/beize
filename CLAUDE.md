# Beize – Übergabe für Claude Code

Einstieg für jede neue Session; ersetzt den Chatverlauf. **Was gerade gilt und was offen ist:
`docs/STATUS.md`.** Was im Code, in den ADRs, im README oder in `docs/` steht, steht hier nicht.

## 1. Was Beize ist

Ein eigenes Betriebssystem auf dem Linux-Kernel: **so konsequent wie macOS, so frei wie
Windows, technisch so offen wie Linux.** Kernel, Treiber und Basisbibliotheken kommen aus
Upstream (selbst gebaut), alles darüber – Compositor, Shell, Einstellungen, Dienste – ist
eigener Code (Rust). **Vor größeren Entscheidungen `docs/VISION.md` lesen.** Meilensteine: M1
QEMU ✓ · M2 echte Hardware ✓ · **M3 Compositor (läuft)** · M4 alltagstauglich · M5 Feinschliff.

## 2. Zusammenarbeit mit dem Besitzer

- **Deutsch**, verständlich, ehrlich über Schwächen („nicht optimiert, bewusst noch nicht“).
- **„Richtig machen statt pfuschen“:** Fehler an der Wurzel beheben und gegen Wiederkehr
  absichern (Test, Compile-Time-Check, Guard in `bz`). Erst den Mechanismus im Quelltext belegen
  (unserer, Upstreams, des Vorbilds), dann ändern; Belegtes von Erschlossenem trennen, sonst
  messen. Symptom-Pflaster entfernen, wenn die Ursache gefunden ist, und das berichten.
- **Nichts Eigenes ohne Vorbild:** Weston, Mutter, KWin, niri, GTK, macOS, Windows nennen. Wenig
  eigener Code: Upstream + Standardkonfiguration. Keine fremden Patches; eigene Upstream-Fixes
  mit Bericht, gesendet vom Besitzer (`kernel/patches/README.md`, `~/upstream/`).
- **Keine getunten festen Zeiten** („damit es geht“): Ereignisse statt Timer, Konzept statt Zahl.
- Neue Architektur: ADR vorab abstimmen. **Werte und Aussehen**, die der Besitzer abgenommen hat,
  ändern sich nur nach seinem Ja (Mock-ups vorher). Bei Problemen einen Schritt zurück,
  Alternativen mit Empfehlung vorlegen.
- **Hardware-Schritte** für den Besitzer nummeriert: wer, wo (WSL/Beize), was er sieht, was ein
  Fehlschlag bedeutet. Aussehen prüft er selbst (Hinweis in der Nachricht), der Hardware-Test
  misst nur.
- Commits und Pushes auf `main` sind freigegeben (englisch, aussagekräftiger Body, Abschluss
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`); ein neuer Signaturschlüssel in
  einen eigenen Commit. **1Password ist tabu** (kein `op`, nie `bz secureboot`, `sign`,
  `release`, `install`). Push hängt, wenn 1Password gesperrt ist: den Besitzer bitten.
- CI nach jedem Push selbst prüfen (`gh run list/view`), ohne Polling; den Nachtlauf zu Beginn.
- Der Besitzer leert den Chat zwischen größeren Schritten: vorher `docs/STATUS.md` aktuell.
- Hilfs-Agenten: für breite Prüfungen erlaubt (`.claude/agents/beize-reviewer.md`); ein Agent
  prüft am Ende jedes größeren Schritts unabhängig gegen.

## 3. Umgebung

- Host Windows 11, Werkbank **WSL2 `Ubuntu-26.04`**, Benutzer `joe`, Repo **`~/beize`**
  (Windows-Pfad `\\wsl.localhost\Ubuntu-26.04\home\joe\beize`). Nicht auf NTFS verschieben.
- GitHub `git@github.com:ScripterCarft/beize.git`; Git in WSL nutzt Windows-`ssh.exe` mit dem
  1Password-SSH-Agent. `gh` ist angemeldet.
- Claude Code auf Windows: Befehle als `WSL_UTF8=1 wsl.exe -d Ubuntu-26.04 -- bash -l -s <<'EOF'
  … EOF` (`bash -c` wird umgeparst). **Backslashes im Heredoc kommen nicht an** und verschachtelte
  Heredocs zerreißen das Skript: Bearbeitungen mit Edit/Write über den Windows-Pfad oder als
  Python-Datei im Scratchpad (`/mnt/c/…`), jede Ersetzung mit `assert`. Edit/Write setzt Modus 644.
- WSL-Home: `~/hw-logs` (Hardware-Ergebnisse), `~/upstream` (Kernel-Bäume, Entwürfe – behalten),
  `~/scratch` (Wegwerf-Arbeit, am Ende des Schritts leeren).

## 4. Architektur

```
Firmware (Secure Boot; PK/KEK/db von Beize + Microsoft im Profil „mit Windows“, ADR 0010)
  └─ systemd-boot (signiert, SBAT „beize“)
       └─ UKI beize_<ver>+3.efi = Kernel + initrd + Kommandozeile + signierte PCR-11-Richtlinie;
          usrhash=<root hash> mount.usr=/dev/mapper/usr …
            └─ initrd (systemd): 1. Start systemd-repart → /usr-Slot B + Root (LUKS2, TPM an
               PCR 7+12 + PCR-11-Richtlinie „initrd“, Recovery-Key), PCR-12-Prüfung,
               dm-verity für /usr, Root entsperren, switch-root
                 └─ /usr (EROFS, dm-verity, read-only) + / (ext4 in LUKS2: /etc, /var, /home)
```

Partitionen, Größen, repart-Definitionen: **nur** `tools/bz/src/layout.rs`. Nur `/usr` wird
ausgeliefert; `/etc` entsteht beim 1. Start aus Links nach `/usr`. Boot-Counting: 3 Versuche;
gut ist ein Start, wenn `beize-boot-health` alle Units aus `running-units.txt` ohne Fehler sieht
(Desktop: auch `beize-compositor-check`, ohne Bildschirm). Sicherheitsmodell: ADR 0005, 0008,
0009, 0010.

## 5. Repository

| Pfad | Inhalt |
| --- | --- |
| `os/` | eigener Rust-Code; je Crate die Moduldoku in `src/main.rs`/`lib.rs` |
| `os/beize-compositor` | Smithay, eigene Bildschleife und Vulkan-Renderer (ADR 0027, 0029) |
| `os/beize-shell`, `beize-ui`, `beize-draw`, `beize-vulkan`, `beize-terminal` | Shell, Toolkit, Zeichnen, GPU, Terminal (ADR 0027 §9, 0028) |
| `os/beize-tpm`, `beize-setup`, `beize-accounts`, `beize-update`, `beize-varlink`, `beize-systemd`, `beize-session` | Start, Konten, Updates, Systemanbindung |
| `os/beize-protocols`, `beize-testclient` | eigene Wayland-Protokolle; Testclient (nur Test-Medium) |
| `tools/bz/src/` | Build-Werkzeug; `vm/smoke/` der Abnahmetest, `desktop/` in Teilen |
| `recipes/*.toml` | ein Rezept pro Paket: Quelle (gepinnt, signiert), `deps`, `[build]`, `[sbom]`, `[files]` |
| `images/<variante>/` | Pakete, Presets, `running-units.txt`, `security.toml`, `sizes.toml` (Ratschen) |
| `system/usr/`, `system/initrd/` | eigene Dateien; initrd ohne coreutils |
| `kernel/config/`, `kernel/patches/` | Fragmente mit Grund; eigene Upstream-Fixes bis Upstream sie hat |
| `ci/` | Buildroot (`buildroot/`), `system.sh` (CI), `measure.sh` (Messbau) |
| `measure/` | nur auf Messzweigen: `hw-test.sh` |

## 6. Bauen, prüfen, freigeben (in `~/beize`)

**Ablauf jeder Änderung – keine Abkürzung:**

1. Ändern; Fehler: erst Mechanismus belegen, dann ändern.
2. `./bz check` – fmt, clippy (Workspace und Compositor allein), Tests, deny; dieselbe Liste wie
   der CI-Job. Der Commit-Hook prüft fmt.
3. `./bz smoke [--only <szenario>]` – baut erst, dann Installation + Szenarien in VMs. `--only`
   nur zum schnellen Nachsehen (ein falscher Name listet alle).
4. Jede neue Prüfung mit **Gegenprobe**: Fix kurz herausnehmen → muss aus dem richtigen Grund rot
   werden. Nie `git checkout` auf Dateien mit ungesicherten Änderungen (Backup der Datei).
5. Commit. Vor dem Push **`./bz gate`** (check + `build --locked` + ganzer Smoke, merkt sich den
   Baum); der Push-Hook lässt nur diesen Baum auf `main` (Doku ausgenommen). Bei Rot: Ursache
   finden, nicht wiederholen, bis es grün ist.
6. Push, dann CI prüfen.

```sh
./bz check · ./bz gate · ./bz smoke [--quick] [--only desktop] · ./bz build
./bz debug 'journalctl -b -p warning' …    # Root-Shell einer VM; --machine <smoke-…> nimmt die
#   liegengelassene Maschine eines roten Laufs; '@wait N' '@screendump build/x.png'
#   '@keys ctrl+alt+t' '@click X Y' · --screen 2560x1440
./bz run · ./bz verify · ./bz outdated · ./bz cve · ./bz package <name> · ./bz -v …
./bz boot-times|hw-check ~/hw-logs/journal
./bz exec <befehl>                         # im Buildroot, z. B. cargo, ci/fuzz.sh
```

- **Echtzeit nur auf Hardware:** In der VM zeichnet lavapipe auf den vCPUs, vblank ist ein Timer,
  vCPUs stehen bis 21 s (#41). Smoke prüft Reihenfolge und Ergebnis, nie Takt; Fristen
  großzügig (`DEADLINE` 60 s). Takt misst der Messbau.
- **Ratschen:** `sizes.toml` (> 3 % bricht; größer mit Grund im Commit), `security.toml` (jede
  Abweichung bricht), `running-units.txt` (genau diese Units), `deny.toml` (Crate-Schichten).
- Ausgabe knapp: bei Fehler die letzten 40 Zeilen; Logs `build/work/logs/<paket>.log`,
  Konsolen `build/work/logs/boot/NN-<maschine>.log`, Journal je Start `.journal.json`. Ein roter
  Desktop-Check gibt Warnungen ins Konsolen-Log; die Maschine bleibt liegen.
- Zeiten: nichts geändert 0,1 s; eine Datei in einer eigenen Crate ~35 s (Cargo-Target bleibt je
  Rezept); Smoke ~4 min, Desktop-Szenario ~190 s (`time:` je Teil); Bau von null ~10 min.
- **Messbau für den PC des Besitzers:** `ci/measure.sh start 0.5.N` (Zweig von `main` mit
  `hw-test.sh` und `kernel/measure` des letzten) → `hw-test.sh` auf das kürzen, was dieser Bau
  misst, Messzeilen `MEASURE 0.5.N` einbauen → `ci/measure.sh finish` (Tag, Bau, Smoke,
  Stick-Dateien) → Besitzer: `./bz release`, „Release fertig“ → `ci/measure.sh stick` (auf D:,
  `cmp`, zurück auf `main`). Ergebnisse: `D:\beize-test\results-*` nach `~/hw-logs`. Messcode
  kommt nie auf `main`.
- **CI** (`.github/workflows/ci.yml`): Buildroot-Image in GHCR; `./bz check` je Push; auf `main`
  `ci/system.sh` mit KVM: jeder Push `SMOKE=quick`, nachts/manuell/wöchentlich voll (wöchentlich
  von null, mit verify, CVE, Fuzzing). Nur-Doku-Pushes bauen nichts. Release nur von einem Commit
  mit grünem vollem Lauf. Runner 4 vCPU/16 GB.

## 7. ADRs (`docs/adr/`)

0001 Kernel aus Upstream · 0002 glibc + systemd · 0003 unveränderliches Image · 0004 WSL2 ·
0005 Cross-Toolchain, Rezepte, signierte Quellen · 0006 Secure Boot, Schlüssel · 0007 nur
`/usr`, A/B, erster Start · 0008 dm-verity · 0009 Root mit LUKS2 + TPM · 0010 Plattform-
Eigentümer, Profile, echte Hardware · 0011 Netzwerk · 0012 Zeit (NTS) · 0013 Was läuft · 0014
Toolchain v3 + Härtung · 0015 Kernel · 0016 initrd aus Funktionen · 0017 Tastatur · 0018 Rechte
(run0, polkit) · 0019 Konten · 0020 Updates · 0021 Buildroot · 0022 SBOM, Schwachstellen · 0023
Absturzberichte · 0024 Bootloader-Konfiguration · 0025 Varianten · 0026 TPM-Schlüssel „(Basis)“ ·
0027 Grafik · 0028 Shell · 0029 Bildschirme. Neue Architekturentscheidung = neues ADR; spätere
Änderungen in die Endfassung einarbeiten, keine Nachträge.

## 8. Regeln für Rezepte und Code

- Nur `/usr` ausliefern; `bz` bricht bei Dateien außerhalb ab.
- **`[files]`** (ADR 0005): jede Datei eines Pakets in `image` oder `build` mit `why`; Neues
  ohne Einordnung lässt den Bau scheitern.
- pkg-config je Maschine; Meson-Rezepte übergeben `--native-file "$MESON_NATIVE"`;
  `meson -Dauto_features=disabled`, jedes Feature ausdrücklich.
- **Strippen ist Sache von `bz`** (MiniDebugInfo, ADR 0023); Rezepte strippen nie.
- Ein Paket baut nur gegen seine (transitiven) `deps`; Compiler nur über `$CC`/`$CXX`; Bauschritte
  mit `MAKEFLAGS="$JOBSERVER"`, nie `-j`; installieren seriell.
- Werkzeuge passend zur systemd-Version (ukify, systemd-measure) aus unserem systemd-Build.
  Host-Werkzeuge nur über `ci/buildroot/packages.txt` + `lock.sh` (alles baut neu: bündeln).
- **Units nur über `images/<variante>/system.preset`/`user.preset`** aktivieren; `running-units.txt`
  mitpflegen; was nur der Desktop braucht, nach `desktop/` (unter `graphical.target`).
- Kein Pfad der Baumaschine im Image (`$PREFIX_MAP`); `bz` prüft.
- **Neuer `beize-*`-Dienst = Eintrag in `security.toml`**, neues Programm = Eintrag in `sizes.toml`.
- **initrd:** Funktionen mit Grund in `images/initrd.toml`; dort kein `sed`/`awk`/`grep`, reines Bash.
- Notfallzugang nur über `beize-tpm emergency`, nie `sulogin`/`debug-shell`.
- Tastatur (ADR 0017): Layout immer als ganze Zeile aus `kbd-model-map` (`localectl … --no-convert`).
- Kernel-Optionen ins passende Fragment, mit Grund (Vorbild Fedora); Treiber als `=m`.
- Neue Quelle: `latest = { release-monitoring = <id> }`, `[sbom]` mit `license` (SPDX) und `cpe`.
- Lücke in einem fremden Rust-Programm: `[cargo.update]` im Rezept, nie den Code patchen.
- **Eigene Crates:** unter `os/<name>`, Rezept wie `recipes/beize-tpm.toml`; keine Build-Skripte
  (Host-cc); Krypto nie selbst; `unsafe` nur mit `// SAFETY:`, kein `unwrap`/`expect` außerhalb
  von Tests; was ein unprivilegierter Aufrufer oder die Firmware schickt, läuft durch einen
  Fuzz-Einstieg (`fuzz/`); Schichten in `deny.toml`. Cargo-Features fügen nur hinzu.
- Ausgaben in `bz` über `say!`; Smoke-Prüfungen belegen, dass ihre Bedingung hergestellt wurde.

## 9. Bekannte Fallstricke

- WSL: nach OOM instabil → `wsl --shutdown`; ohne angehängte `wsl.exe`-Sitzung wird die Distro
  beendet (lange Jobs im Hintergrund mit Meldung, Logs nach `~`). `C:\Users\Joe\.wslconfig`
  `kernelCommandLine = preempt=full` (ohne: Free Page Reporting hielt CPUs bis 141 ms; `bz smoke`
  warnt). Der Stick D: ist in WSL nicht eingehängt: `sudo -n mount -t drvfs D: /mnt/d`.
- `./bz build` liest stdin: in Skripten `</dev/null`. `qemu -serial stdio` ebenso.
- Host-`cargo check` linkt nicht: fehlende C-Symbole (z. B. `sd_network_*`) erst in `bz build`.
- CI rot, lokal grün: Runner (4 vCPU, root, ohne Service-Manager, von null).
- Bit-gleiche Bauten: Zeiten über `SOURCE_DATE_EPOCH`/`BUILD_EPOCH`.
- systemd-Konsolenzeilen mit ANSI-Codes mitten im Text (`strip_ansi`); der Kernel schreibt auch
  mitten in Zeilen (`whole_line`).
- Smoke tippt über QMP echte Tasten: vor dem Öffnen von tty1 gehen sie verloren (`PASSWORD_PAUSE`);
  Haltezeiten mit `console::hold`, nicht `@keys`; gleiche Mausposition ist kein Ereignis.
- `bz debug` überschreibt `logs/boot`; `--machine` bootet die Disk der liegengelassenen Maschine.
- Rust: `fs::copy` schreibt sparse Dateien voll (`copy_sparse`); `Command::output()` setzt stdin
  auf null; per `kill` geschicktes SIGSEGV verpufft (SIGABRT nehmen).
- Vulkan: die `Instance` muss das Gerät überleben. Keine Validation-Layer im Image.
- systemd: an sd-event gehängte Verbindung schließt mit der Schleife; nach `StartUnit` auf
  `JobRemoved` warten; `ProtectHome=yes` sperrt `/run/user`; `systemctl is-active` ist bei
  „deactivating“ ungleich 0; `StateDirectory=` und `PrivateTmp=yes` ordnen nach tmpfiles (früh
  laufende Units: `ReadWritePaths=`, `PrivateTmp=disconnected`). run0 kann die letzte Ausgabe
  verlieren (262.1, `ptyfwd.c` `drained()`): im Test über eine Datei lesen.
- Smithay: `DrmCompositor` hält Client-Puffer nicht bis zum Flip; Popup-Grabs lassen erst beim
  nächsten Ereignis los (`let_go_of`).
- nouveau/NVK: Flip-Ereignisse wiederholen manchmal den vblank davor; ~100-ms-Stau der GSP;
  keine GPU-Priorität; nach Leerlauf braucht die GPU ~1,5 s bis zum vollen Takt.
- Terminal-Fehler: erst prüfen, was bash/readline linkt (ncurses), dann das Terminal.

## 10. Offene Punkte

`docs/STATUS.md` (Stand, nächste Schritte, offene Funde mit Ort) und GitHub Issues
(`gh issue list`; Labels `m3-plan`, `upstream`, `hardware`). Ein Commit, der ein Issue erledigt,
nennt `Fixes #N`. Upstream-Entwürfe in WSL `~/upstream/` (sendet der Besitzer).
