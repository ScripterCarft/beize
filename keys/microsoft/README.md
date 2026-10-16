# Microsoft-Zertifikate für Secure Boot

Öffentliche Zertifikate und die Sperrliste von Microsoft, die im Profil
„Beize + Windows“ neben Beizes eigenen Schlüsseln in die Firmware kommen
(ADR 0010). `bz` prüft jede Datei gegen die SHA-256-Werte in
`tools/bz/src/secureboot/mod.rs` und bricht bei einer Abweichung ab.

Quelle: <https://github.com/microsoft/secureboot_objects>, Commit
`106458910883aef8836aef293d1e4e43803a7be0` (2026-10-06). Gegengeprüft gegen die
Zertifikate in Ubuntus OVMF-Paket (`OVMF_VARS_4M.ms.fd`): alle bis auf die
Option ROM CA 2023 (dort noch nicht enthalten) sind identisch.

| Datei | Variable | Zertifikat | Wozu |
| --- | --- | --- | --- |
| `kek-ca-2011.der` | KEK | Microsoft Corporation KEK CA 2011 | Windows spielt db/dbx-Updates ein (alt) |
| `kek-2k-ca-2023.der` | KEK | Microsoft Corporation KEK 2K CA 2023 | dasselbe, Nachfolger |
| `windows-production-pca-2011.der` | db | Microsoft Windows Production PCA 2011 | Windows-Bootmanager (alt) |
| `windows-uefi-ca-2023.der` | db | Windows UEFI CA 2023 | Windows-Bootmanager (neu) |
| `uefi-ca-2011.der` | db | Microsoft Corporation UEFI CA 2011 | Option-ROMs (z. B. Grafikkarte), Fremd-Bootloader (alt) |
| `uefi-ca-2023.der` | db | Microsoft UEFI CA 2023 | Fremd-Bootloader (neu) |
| `option-rom-uefi-ca-2023.der` | db | Microsoft Option ROM UEFI CA 2023 | Option-ROMs (neu) |
| `dbx-update-amd64.bin` | dbx | Sperrliste, von Microsofts KEK signiert | gesperrte, angreifbare Bootloader bleiben gesperrt |

Aktualisieren: Dateien aus einem neueren Commit holen, Prüfsummen in
`tools/bz/src/secureboot/mod.rs` und Commit hier anpassen, `./bz smoke` (prüft das Einschreiben).
Die neuen Einschreib-Dateien für echte Hardware erzeugt danach der Besitzer.
