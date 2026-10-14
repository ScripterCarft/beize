# 0015 – Kernel: Konfiguration, Module, Microcode, Firmware

Status: angenommen · 2026-10-10

## Kontext

Der Kernel entstand aus `defconfig` + `kvm_guest.config` + zwei eigenen Fragmenten.
`defconfig` ist eine Entwickler-Konfiguration: Was darin an- oder ausgeschaltet ist,
hat niemand für Beize entschieden (es fehlten z. B. die PCIe-Dienste `HOTPLUG_PCI_PCIE`,
`PCIEAER`, `ACPI_PCI_SLOT`, die jede Distribution einschaltet). Alle Treiber waren fest
eingebaut und wurden auf jedem Rechner initialisiert, ob das Gerät existiert oder
nicht. Fest eingebaut kann ein Treiber auch Geräte binden, die einem anderen gehören:
`mei_me` belegte auf dem Rechner des Besitzers 5,7 s lang den xHCI-Controller.
Microcode und Firmware lieferte Beize nicht aus.

## Entscheidung

- **Version: neuester Stable**, nicht LTS (Besitzer) – Geräteunterstützung und
  Korrekturen zuerst. Fehler im Kernel gehen „upstream first“: Bericht und Patch an
  die Maintainer, kein eigener Patch im Rezept. `bz outdated` meldet neuere Releases
  aller Quellen; jede Quelle nennt im Rezept, wo sie angekündigt werden (`latest`):
  release-monitoring.org wie bei Fedora, für den Kernel kernel.org selbst (dort stehen
  die Punkt-Releases zuerst).
- **Konfiguration eigen, auf Upstreams Standardwerten:** Basis ist `alldefconfig` –
  jede Option mit dem Standard, den ihr Maintainer festgelegt hat –, darauf das
  upstream gepflegte `hardening.config` und Beizes nach Funktion gegliederte Fragmente
  in `kernel/config/` (Kern, Plattform, Sicherheit, Speicher, Netz, Geräte), jede
  Option mit Begründung. Nicht `allnoconfig`: Das verneint auch Upstreams
  Sicherheitsstandards (im Versuch waren alle CPU-Mitigations aus). Ausnahmen von
  `hardening.config` stehen mit Grund im Fragment (kCFI und Stack-Erasing brauchen
  clang oder gcc-Plugins, die Beize wie Fedora nicht nutzt; kein `/dev/mem` statt eines
  eingeschränkten). Die aufgelöste `.config` liegt
  als `kernel/beize.config` im Repo (ohne die Versionsnummern der Werkzeuge); `bz`
  schreibt sie bei Änderung neu, `bz --locked` (CI) bricht dann ab. Ein Versionssprung
  zeigt so jede neue oder geänderte Option im Diff (Vorbild: Fedora entscheidet jede
  Option ausdrücklich, Android GKI und ChromeOS führen ihre Konfiguration vollständig).
  `bz` bricht ab, wenn Kconfig eine gewünschte Option verwirft oder es sie nicht gibt.
- **Compiler: Beizes Toolchain** (gcc, binutils), dieselbe wie für jedes Paket – wie
  Yocto, Buildroot und Fedora. Nur kbuilds Hilfsprogramme, die beim Bau laufen, nimmt
  der Host-Compiler. Out-of-tree-Module (NVIDIA) entstehen so mit dem Compiler des
  Kernels. Bis 2026-10-14 baute unbemerkt der gcc des Buildroots (Ubuntu) den Kernel (#29).
- **Treiber als Module, fest nur Infrastruktur:** Fest eingebaut ist, was jeder Start
  auf jedem Beize-Rechner braucht, bevor udev Module laden kann oder unabhängig vom
  Gerät: EFI, initrd, Konsole (seriell, simpledrm, fbcon), EROFS, ext4, dm-verity,
  dm-crypt, Krypto, TPM, xHCI mit USB-HID und PS/2 (Tastatur für den Recovery-Key; wie
  Fedora – damit gewinnt xHCI auch die Modalias, die `mei_me` fälschlich für denselben
  Controller meldet). Alles Gerätespezifische (NVMe, AHCI, virtio, Netzwerk, Chipsatz,
  Sensoren …) ist Modul; udev lädt, was die Hardware hat. In die initrd kommen nur die
  Plattencontroller aus `images/initrd.toml` samt Abhängigkeiten. Werkzeuge: kmod,
  systemd mit `kmod` und `acl`.
- **Module signiert und erzwungen** (`MODULE_SIG_FORCE`), ML-DSA-87 wie Fedora (dazu wie
  dort `PKCS7_WAIVE_AUTHATTRS_REJECTION_FOR_MLDSA`: OpenSSL < 4.0 signiert ML-DSA nur
  mit Attributen), zstd-komprimiert, vom Kernel entpackt, unter dm-verity in
  `/usr/lib/modules`. **Signiert wird getrennt vom Bau**, wie Upstream es für
  reproduzierbare Kernel beschreibt (`Documentation/kbuild/reproducible-builds.rst`)
  und Debian es macht (`linux-signed`): Jeder Schlüsselsatz (ADR 0010) hat einen
  Modul-Schlüssel; der Kernel trägt nur dessen Zertifikat (`SYSTEM_TRUSTED_KEYS`),
  kbuild erzeugt keinen Schlüssel und signiert nichts. Kernel und Module entstehen
  unsigniert und auf jedem Rechner gleich; `bz` signiert die Module beim Zusammensetzen
  eines Images für dessen Schlüssel (Test-Schlüssel beim Bau, Produktions-Schlüssel
  bei `bz sign`/`bz release`, privater Teil nur in 1Password) und linkt für fremde
  Schlüssel nur den Kernel neu. ML-DSA signiert mit Zufall: Signaturen werden wie
  Gebautes aufbewahrt (einmal je Kernel-Bau und Schlüsselsatz). Die Module der initrd
  liegen als zweites, unkomprimiertes Archiv im UKI. Module außerhalb des Kernelbaums
  (NVIDIA, später) signiert `bz` genauso.
- **Lockdown „integrity“ immer** (`LOCK_DOWN_KERNEL_FORCE_INTEGRITY`; Fedora und Ubuntu
  schalten es unter Secure Boot ein, Beize startet immer mit Secure Boot): Auch root kann
  den laufenden Kernel nicht verändern – kein unsigniertes Modul, kein `/dev/mem`, kein
  unsigniertes kexec. Erst damit schützt die Modulsignatur.
- **Microcode im UKI** (`.ucode`-Abschnitt, ukify): Intel (`intel-ucode/`, ohne
  „with-caveats“, wie Arch und Fedora) und AMD (aus linux-firmware), für jede CPU, auf
  der Beize läuft (x86-64-v3), ohne Server: Xeons, ältere CPUs und Atoms ohne AVX2
  bleiben draußen (Ausschlussliste in `bz`, damit neue CPUs ohne Änderung dabei sind).
  Der Kernel spielt ihn ein, bevor er sonst etwas tut; ein Microcode-Update ist Teil
  des signierten Images.
- **Firmware aus linux-firmware, nur was ein gebautes Modul anfordert**
  (`modinfo -F firmware`). Bisher fordert keines welche an; `bz` bricht ab, sobald eines
  es tut – die Auswahl kommt mit dem ersten solchen Treiber (WLAN, Bluetooth, GPU).
- **Prüfungen:** `bz smoke` verlangt Lockdown „integrity“, einen Treiber für jedes
  PCI-Gerät der VM (Brücken ausgenommen) und dass der Kernel ein unsigniertes Modul
  abweist (per fw_cfg hineingereicht), dasselbe Modul signiert aus dem Image aber lädt.
  `bz hw-check <journal>` zeigt vor einer Installation, welches PCI-Gerät eines echten
  Rechners im Image einen Treiber hat.

## Konsequenzen

- Geräteunterstützung zuerst für den Rechner des Besitzers und die VMs (VISION:
  „keine beliebige Hardware“); jede weitere Geräteklasse ist ein Fragment. Auf dem
  Rechner des Besitzers fehlen bewusst noch WLAN, Ton und die NVIDIA-GPU (mit ihren
  Phasen); SPI-Flash (BIOS) und Shared SRAM brauchen keinen Treiber.
- Kernel 10 MB statt 16 MB, 60 Module (2,2 MB); in der VM laden 28 davon. Er
  initialisiert nur noch, was da ist: weniger Angriffsfläche, kürzerer Kernel-Start.
- Module von Dritten, die `bz` nicht baut, lassen sich nicht laden (wie
  Treibersignatur unter Windows). Lockdown verbietet Ruhezustand auf Platte
  (Hibernation); Standby ist nicht betroffen.
- Speicher wird beim Anfordern **und** beim Freigeben genullt (`INIT_ON_FREE` aus
  `hardening.config`, ausdrücklich behalten; Fedora und Ubuntu nur beim Anfordern):
  Kein alter Inhalt überlebt im freien Speicher, Use-after-free liest Nullen statt
  fremder Daten. Kostet beim Start ~2 s auf 32 GB, weil der Kernel einmal den ganzen
  RAM löscht (Entscheidung des Besitzers).
- Der Microcode macht das UKI 6,6 MB größer (alle Intel-Updates wären 17,3 MB, davon
  10 MB Server); der Loader brauchte auf der Hardware trotzdem nur 0,65 s.
- linux-firmware ist 632 MB groß und wird nie ganz entpackt; `bz` holt nur, was es
  braucht.
- Reproduzierbar ist alles Unsignierte: Module, initrd und `/usr` sind auf jedem
  Rechner bit-gleich (`ci/digest.sh` zeigt es in CI und lokal); der Kernel unterscheidet
  sich nur im Zertifikat. Signaturen sind Eingaben wie bei Debian: dasselbe Image
  entsteht wieder aus demselben Bau mit denselben Signaturen.
- Ein Modul-Schlüssel gilt dauerhaft; wer ihn hat, kann Module für jeden Beize-Kernel
  dieses Schlüsselsatzes signieren. Er liegt deshalb wie db- und PCR-Schlüssel nur in
  1Password (der frühere Wegwerf-Schlüssel je Bau verhinderte jede Reproduzierbarkeit).
  Den Produktions-Schlüssel legt der Besitzer einmal an (`bz secureboot new-module-key`).
