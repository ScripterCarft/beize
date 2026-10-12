# 0014 – Toolchain: x86-64-v3 und Härtung

Status: angenommen · 2026-10-10

## Kontext

Beize baute alles mit `-O2 -pipe` für die Grundstufe x86-64 (2003) – ohne AVX2,
BMI2, FMA, die jede CPU seit 2013/2015 hat. Von der üblichen Härtung waren nur PIE und
der Stack-Protector an (Compiler-Standard); es fehlten FORTIFY, Schutz gegen Stack-Clash,
Control-flow Enforcement (CET), sofortiges Binden mit schreibgeschützter
Relokationstabelle (`-z now`) und Frame-Pointer. Ob ein Paket die Flags überhaupt
übernimmt, prüfte niemand.

## Entscheidung

- **Zielarchitektur x86-64-v3, Tuning `generic`** – wie RHEL 10, dessen Grundstufe v3
  ist. `gcc` (beide Stufen) wird mit `--with-arch=x86-64-v3 --with-tune=generic`
  gebaut: Die Stufe ist Standard des Compilers und gilt auch für Pakete, die Flags
  überschreiben, und für gcc's eigene Laufzeitbibliotheken. `generic` statt
  Raptor-Lake-Tuning: Die CPU des Besitzers hat zwei Kerntypen, `generic` ist auf
  beide abgestimmt, und Beize bleibt auf jeder v3-CPU gleich schnell. Rust:
  `-C target-cpu=x86-64-v3`.
- **Härtung wie Fedora** (`redhat-rpm-config`, die Flags der größten Distribution mit
  dem strengsten Standard), für C und C++:
  `-D_FORTIFY_SOURCE=3 -D_GLIBCXX_ASSERTIONS -fstack-protector-strong
  -fstack-clash-protection -fcf-protection -fexceptions -fasynchronous-unwind-tables
  -fno-omit-frame-pointer -mno-omit-leaf-frame-pointer -Werror=format-security`,
  gelinkt mit `-z relro -z now --as-needed -z pack-relative-relocs`. gcc's
  Laufzeitbibliotheken mit `--enable-cet`; glibc mit seinen eigenen Schaltern
  (`--enable-fortify-source=3`, CET folgt dem Compiler). Rust: `-C
  force-frame-pointers=yes`; PIE und `relro`/`now` sind dort Standard.
- **Frame-Pointer** kosten ~1 % und machen jedes Profil vollständig (Fedora 38, Ubuntu
  24.04): „erst messen, dann optimieren“ braucht sie.
- **Nicht übernommen:** LTO und `-O3` – Leistungsentscheidungen, die erst nach
  Messungen auf der Hardware fallen.
- **ELF-Prüfung in `bz`** (Vorbild: Fedoras annocheck): Jede ELF-Datei in `/usr` und in
  der initrd muss PIE bzw. eine Bibliothek sein, `BIND_NOW` und `GNU_RELRO` haben, einen
  nicht ausführbaren Stack und keine Text-Relokationen. C/C++ muss zudem die
  CET-Eigenschaften (IBT, SHSTK) tragen – die fehlen, sobald ein einziges Objekt ohne
  Beizes Flags gebaut wurde, und machen so Pakete sichtbar, die die Flags ignorieren.
  Ausnahmen stehen mit Grund in `bz` (`NO_CET`); jede Verletzung bricht den Build. Jede
  Datei braucht außerdem eine GNU-Build-ID (wie Fedora): Sie benennt den Build in
  Core-Dumps und Absturzberichten, und ihr Fehlen zeigt, dass nicht Beizes Compiler
  gelinkt hat – bis 2026-10-12 baute so unbemerkt der Bootstrap-Compiler gcc-pass1 alle
  Pakete, weil er im selben Präfix den endgültigen gcc überschrieben hatte; seitdem hat
  er ein eigenes (`build/tools/pass1`), das nur glibc nutzt. Die
  Stufe x86-64-v3 prüft glibc selbst: Die Programme tragen die Markierung „ISA needed:
  x86-64-v3“, die der Linker aus dem Code ableitet.
- **Shadow Stacks im Kernel** (`CONFIG_X86_USER_SHADOW_STACK`, wie Fedora und Ubuntu):
  Erst damit wirkt CET für Programme. Der Boot-Report meldet, ob PID 1 mit Shadow Stack
  läuft; `bz smoke` verlangt es, wo die CPU ihn hat.
- **systemd mit libseccomp** (wie Fedora und Arch): Erst damit wirken die
  Abschottungen der Units, die über Systemaufruf-Filter laufen (`SystemCallFilter=`,
  `RestrictAddressFamilies=`, `MemoryDenyWriteExecute=` …) – ohne sie ignoriert systemd
  sie still; so war es bis 2026-10-11. Auch in der initrd. `bz smoke` prüft, dass
  journald und ntpd-rs im Filtermodus laufen (`Seccomp: 2`).

## Konsequenzen

- Beize startet nur auf CPUs mit x86-64-v3 (Intel ab Haswell 2013, AMD ab Excavator
  2015). Auf älteren bricht glibc beim Start mit „CPU ISA level is lower than
  required“ ab – deutlich, nicht mit zufälligen Abstürzen. Die VMs laufen mit der
  CPU des Rechners (`-cpu host`), die CI-Runner haben v3.
- Rust (stabil) kann keinen CET-Code erzeugen; ein Prozess erhält Shadow Stacks nur,
  wenn alle seine Objekte sie unterstützen. Die Rust-Programme (beize-tpm,
  beize-setup, ntpd-rs) laufen deshalb ohne – Ausnahme in der ELF-Prüfung, bis Rust es
  stabil anbietet.
- Vorher hatten 555 von 728 ELF-Dateien in `/usr` kein `BIND_NOW` und keine CET; jetzt
  erfüllen alle 728 (und alle 356 der initrd) die Prüfung. Die Startzeiten in der VM
  sind unverändert – der Start wartet auf Geräte, TPM und Platte, nicht auf Rechenzeit.
