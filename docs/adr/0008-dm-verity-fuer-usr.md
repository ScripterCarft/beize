# 0008 – dm-verity für /usr

Status: angenommen · 2026-10-07

## Kontext

Secure Boot (ADR 0006) schützt Bootloader, Kernel, initrd und Kommandozeile.
`/usr` – das eigentliche Betriebssystem – lag bisher ungeschützt auf der Disk:
wer die Partition verändert, verändert das System.

## Entscheidung

- Jeder `/usr`-Slot besteht aus zwei Partitionen: den EROFS-Daten und dem
  dm-verity-Hash-Baum (SHA-256, 4-KiB-Blöcke). `bz usr` erzeugt beides; der Salt
  wird aus dem Image abgeleitet, der Root-Hash ist damit reproduzierbar.
- Der Root-Hash steht als `usrhash=` in der Kernel-Kommandozeile des signierten
  UKI. Die Partitions-UUIDs sind die beiden Hälften des Root-Hashes, systemd
  findet so genau den Slot, der zum UKI passt (UAPI.2). Jeder gelesene Block
  wird gegen den Hash-Baum geprüft.
- Damit reicht die Vertrauenskette lückenlos von der Firmware bis zur letzten
  Datei in `/usr`: Firmware prüft systemd-boot und UKI, das UKI enthält den
  Root-Hash, der Kernel prüft `/usr`.
- `libcryptsetup` (mit OpenSSL, json-c und libdevmapper aus LVM2) kommt dafür
  ins System; dieselben Bibliotheken braucht die Verschlüsselung der
  Root-Partition. json-c ist die einzige Quelle ohne Upstream-Signatur und als
  begründete Ausnahme markiert (`keys/README.md`).
- Alle Partitionen außer Root haben eine feste Größe; sonst lässt systemd-repart
  bestehende Partitionen in den freien Platz wachsen.

## Konsequenzen

- `bz smoke` verlangt `usr=verity` und prüft die Gegenrichtung: ein einzelnes
  verändertes Byte in `/usr` wird erkannt, das System bootet nicht davon.
- Ein beschädigter Block führt zu einem Lesefehler. Ob das System in diesem Fall
  neu startet (und nach drei Versuchen auf den anderen Slot zurückfällt), wird
  mit dem Updater entschieden.
