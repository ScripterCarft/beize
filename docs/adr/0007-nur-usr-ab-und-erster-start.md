# 0007 – Nur /usr ausliefern, A/B-Slots, Einrichtung beim ersten Start

Status: angenommen · 2026-10-07 · ersetzt die Schreibschutz-Root aus ADR 0003

## Kontext

Bisher war das ganze Root-Dateisystem ein schreibgeschütztes Image, `/etc` lag
darin, `/var` war flüchtig. Damit lassen sich weder Einstellungen dauerhaft
speichern noch sauber Updates einspielen. Gleichzeitig soll das System nie in
einen halb kaputten Zustand geraten und sich jederzeit auf Werkseinstellungen
zurücksetzen lassen.

## Entscheidung

Beize folgt dem von systemd empfohlenen Aufbau (`docs/ROOTFS_DISCOVERY.md`,
`docs/BUILDING_IMAGES.md` im systemd-Quellcode):

| Partition | Inhalt | Entsteht |
| --- | --- | --- |
| ESP | systemd-boot, UKIs (signiert) | im Image |
| `/usr` Slot A | das komplette Betriebssystem, EROFS, schreibgeschützt | im Image |
| `/usr` Slot B | Platz für das nächste Update | erster Start |
| Root `/` | `/etc`, `/var`, `/home` – alles, was dem Besitzer gehört | erster Start |

- **Nur `/usr` wird ausgeliefert.** `bz` bricht ab, wenn ein Paket etwas
  außerhalb von `/usr` installieren will. Beizes eigene Vorgaben für Programme, die
  in `/etc` lesen (`profile`, `ld.so.conf`, `nanorc`, `ntp.toml`, die Zertifikate),
  sind Links nach `/usr` (`tmpfiles.d/beize.conf`). So bringt jede Version ihre eigenen
  mit, wie bei der Bootloader-Konfiguration (ADR 0024). Der Besitzer ersetzt einen Link
  durch eine Datei, wenn er etwas ändern will, und die bleibt dann. Kopiert würde eine
  Vorgabe nie wieder erneuert: Das war bis 0.4 der Fall bei `profile` und `ld.so.conf`.
  systemds eigene Vorgaben kopiert systemd beim ersten Start aus `/usr/share/factory/etc`
  (`nsswitch.conf`, `pam.d`). systemd erzeugt den Rest selbst (Machine-ID, Benutzer,
  Symlinks).
- **Die initrd im UKI** legt beim ersten Start mit systemd-repart Slot B und
  die Root-Partition an (Definitionen erzeugt aus `tools/bz/src/layout.rs`),
  findet die Root-Partition auf der Boot-Disk und hängt `/usr` des eigenen
  Slots ein. Die Kernel-Kommandozeile bindet jedes UKI an genau seinen Slot.
- **Boot-Counting:** Ein neues UKI heißt `beize_<version>+3.efi`. Erreicht das
  System `boot-complete.target`, entfernt systemd-bless-boot den Zähler.
  Scheitert der Start dreimal, wählt systemd-boot die vorherige Version.
- **Werksreset** = Root-Partition leeren (`FactoryReset=yes`); `/usr` bleibt unberührt.

## Konsequenzen

- Das ausgelieferte Image ist klein und enthält keine Daten des Besitzers.
- Ein Update ist ein neuer `/usr`-Slot plus ein neues UKI; `/etc` und `/var`
  bleiben, das alte System bleibt als Rückfall erhalten.
- `bz smoke` installiert das Image auf eine frische Test-Disk, bootet zweimal
  und prüft Partitionen, Boot-Counting und dass der Zustand den Neustart übersteht.
- dm-verity für `/usr` (ADR 0008) und die Verschlüsselung der Root-Partition
  (ADR 0009) bauen darauf auf; offen ist der Updater (systemd-sysupdate).
