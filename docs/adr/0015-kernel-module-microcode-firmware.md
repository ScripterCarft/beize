# 0015 – Kernel: Konfiguration, Module, Microcode, Firmware

Status: angenommen · 2026-10-10

## Kontext

Der Kernel entsteht aus `defconfig` + `kvm_guest.config` + zwei eigenen Fragmenten.
`defconfig` ist eine Entwickler-Konfiguration: Was darin an- oder ausgeschaltet ist,
hat niemand für Beize entschieden (es fehlen z. B. die PCIe-Dienste `HOTPLUG_PCI_PCIE`,
`PCIEAER`, `ACPI_PCI_SLOT`, die jede Distribution einschaltet). Alle Treiber sind fest
eingebaut und werden auf jedem Rechner initialisiert, ob das Gerät existiert oder
nicht. Fest eingebaut kann ein Treiber auch Geräte binden, die einem anderen gehören:
`mei_me` belegt auf dem Rechner des Besitzers 5,7 s lang den xHCI-Controller. Microcode
und Firmware liefert Beize nicht aus.

## Entscheidung

- **Version: neuester Stable**, nicht LTS (Besitzer) – Geräteunterstützung und
  Korrekturen zuerst. `bz outdated` meldet neuere Versionen aller Rezepte. Fehler im
  Kernel gehen „upstream first“: Bericht und Patch an die Maintainer, kein eigener
  Patch im Rezept.
- **Konfiguration vollständig eigen:** Basis `allnoconfig`, darauf nach Funktion
  gegliederte Fragmente in `kernel/config/` (Plattform, Speicher, Netz, Sicherheit,
  Geräteklassen …) plus das upstream gepflegte `hardening.config`. Nichts gilt, weil
  es Standard von irgendwem ist. Die aufgelöste `.config` liegt als
  `kernel/beize.config` im Repo; `bz` bricht ab, wenn sie vom Ergebnis abweicht. Ein
  Versionssprung zeigt so jede neue oder geänderte Option im Diff (Vorbild: Fedora
  entscheidet jede Option ausdrücklich, Android GKI und ChromeOS führen ihre
  Konfiguration vollständig).
- **Treiber als Module, fest nur Infrastruktur:** Fest eingebaut ist, was jeder Start
  auf jedem Beize-Rechner braucht, bevor udev Module laden kann oder unabhängig vom
  Gerät: EFI, initrd, Konsole (seriell, simpledrm, fbcon), EROFS, ext4, dm-verity,
  dm-crypt, Krypto, TPM, xHCI mit USB-HID (Tastatur für den Recovery-Key; wie Fedora –
  damit gibt es auch keinen Wettlauf zweier Module um dieselbe Modalias wie bei
  `mei_me`/xHCI). Alles Gerätespezifische (NVMe, AHCI, virtio, Netzwerk, Grafik, Ton …)
  ist Modul; udev lädt per Modalias, was die Hardware hat. Speichertreiber liegen
  zusätzlich in der initrd. Werkzeuge: kmod, systemd mit `kmod` und `acl`.
- **Module signiert und erzwungen** (`MODULE_SIG_FORCE`), zstd-komprimiert, vom Kernel
  entpackt, unter dm-verity in `/usr/lib/modules`. Der Schlüssel wird bei jedem
  Kernel-Bau neu erzeugt und danach gelöscht – nie ausgeliefert, nie im CI-Cache (wie
  Arch). Module außerhalb des Kernelbaums (NVIDIA, später) baut und signiert `bz` im
  selben Schritt.
- **Lockdown „integrity“ immer** (`LOCK_DOWN_KERNEL_FORCE_INTEGRITY`; Fedora und Ubuntu
  schalten es unter Secure Boot ein, Beize startet immer mit Secure Boot): Auch root kann
  den laufenden Kernel nicht verändern – kein unsigniertes Modul, kein `/dev/mem`, kein
  unsigniertes kexec. Erst damit schützt die Modulsignatur.
- **Microcode im UKI** (`.ucode`-Abschnitt, ukify): Intel und AMD; der Kernel lädt ihn,
  bevor er sonst etwas tut. Ein Microcode-Update ist damit Teil des signierten Images.
- **Firmware aus linux-firmware, nur was ein gebautes Modul anfordert**
  (`modinfo -F firmware`), zstd-komprimiert unter `/usr/lib/firmware`. Fehlt eine
  angeforderte Datei, bricht `bz` ab.
- **Prüfungen:** `bz smoke` belegt, dass udev die Module der VM lädt, jedes PCI-Gerät
  der VM einen Treiber hat, ein unsigniertes Modul abgewiesen wird und Lockdown aktiv
  ist. `bz hw-check <journal>` prüft vor einer Installation, dass jedes PCI-Gerät aus
  dem Journal eines echten Rechners einen Treiber im Image hat.

## Konsequenzen

- Geräteunterstützung zuerst für den Rechner des Besitzers und die VMs (VISION:
  „keine beliebige Hardware“); jede weitere Geräteklasse ist ein Fragment.
- Der Kernel initialisiert nur noch, was da ist: weniger Startzeit, weniger
  Angriffsfläche.
- Module von Dritten, die `bz` nicht baut, lassen sich nicht laden (wie
  Treibersignatur unter Windows). Lockdown verbietet Ruhezustand auf Platte
  (Hibernation); Standby ist nicht betroffen.
- Der Wegwerf-Schlüssel macht Module nicht bit-genau reproduzierbar (offener Punkt
  „bit-genaue Builds“). Sobald Upstream eine Prüfung per Modul-Hash statt
  Signatur anbietet (7.2 hat keine), wechselt Beize dorthin.
