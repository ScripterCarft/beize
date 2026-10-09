# Signaturschlüssel

Jede Quelle in `recipes/` muss von ihren Upstream-Maintainern signiert sein.
`bz` prüft die Signatur mit `gpgv` gegen genau einen Schlüsselbund aus diesem
Verzeichnis. Jeder Bund enthält nur die Schlüssel, die das Projekt tatsächlich
signieren (`gpg --export --export-options export-minimal`).

Prüfen aller Quellen: `./bz verify`

| Schlüsselbund | Signiert von | Fingerprint | Bezogen über |
| --- | --- | --- | --- |
| `bash` | Chet Ramey | `7C01 35FB 088A AF6C 66C6 50B9 BB58 69F0 64EA 74AB` | GNU-Keyring |
| `binutils` | Nick Clifton | `3A24 BC1E 8FB4 09FA 9F14 3718 13FC EF89 DD9E 3C4F` | GNU-Keyring |
| `coreutils` | Pádraig Brady | `6C37 DC12 121A 5006 BC1D B804 DF6F D971 3060 37D9` | GNU-Keyring |
| `cryptsetup` | Milan Broz | `2A29 1824 3FDE 4664 8D06 86F9 D9B0 577B D93E 98FC` | kernel.org pgpkeys-Repository |
| `dbus` | Simon McVittie | `7A07 3AD1 AE69 4FA2 5BFF 62E5 235C 099D 3EB3 3076` | keyserver.ubuntu.com |
| `e2fsprogs` | Theodore Ts'o | `2B69 B954 DBFE 0879 2881 37C9 F2F9 5956 950D 81A3` | keys.openpgp.org |
| `expat` | Sebastian Pipping | `CB8D E70A 90CF BF6C 3BF5 CC56 9626 2ACF FBD3 AEC6` | keyserver.ubuntu.com |
| `gcc` | Richard Biener | `7F74 F97C 1034 68EE 5D75 0B58 3AB0 0996 FC26 A641` | GNU-Keyring |
| `glibc` | Andreas K. Hüttel | `35B1 7DF5 7525 77CA 0C54 1CEB 94BF DF44 84AD 142F` | keys.openpgp.org (GNU-Keyring enthält eine abgelaufene Fassung) |
| `gmp` | Niels Möller | `343C 2FF0 FBEE 5EC2 EDBE F399 F359 9FF8 28C6 7298` | GNU-Keyring |
| `json-c` | Upstream signiert weder Release-Archive noch Tags (geprüft 2026-10-07) | Commit des Release-Tags gepinnt; derselbe Tag, aus dem Arch Linux baut |
| `libcap` | Andrew G. Morgan | `38A6 4469 8C69 7873 44E9 54CE 29EE 848A E2CC F3F4` | WKD kernel.org |
| `libxcrypt` | Björn Esser | `678C E3FE E430 3115 96DB 8C16 F52E 9800 7594 C21D` | keyserver.ubuntu.com; deckungsgleich mit `libxcrypt-gpgkey.asc` im Release |
| `linux` | Greg Kroah-Hartman | `647F 2865 4894 E3BD 4571 99BE 38DB BDC8 6092 693E` | WKD kernel.org |
| `linux-pam` | Dmitry V. Levin | `7BEC FE3A F7B2 80BB 52FF 77F1 04BA 4521 C996 DDE1` | keyserver.ubuntu.com; signiert mit Unterschlüssel `FB3D CD19 FF87 4735 E4F6 1CE6 4E9F E852 2F16 0EC2` |
| `lvm2` | Marian Csontos | `D501 A478 440A E2FD 130A 1BE8 B911 2431 E509 039F` | keys.openpgp.org |
| `mpc` | Andreas Enge | `AD17 A21E F8AE D8F1 CC02 DBD9 F7D5 C9BF 765C 61E3` | GNU-Keyring |
| `mpfr` | Vincent Lefèvre | `A534 BE3F 83E2 41D9 1828 0AEB 5831 D11A 0D4D B02A` | GNU-Keyring |
| `openssl` | OpenSSL | `C46E D3F2 CBEF DA1F DAAD A442 64ED 7B1D CCE7 1CB2` | openssl-library.org/source/pubkeys.asc |
| `systemd` | Luca Boccassi | `AC24 AAC7 DDC4 20F1 8ECA 6B91 286B F7EF CD77 241E` | keyserver.ubuntu.com, signierter Git-Tag |
| `tpm2-tss` | Andreas Fuchs (Infineon) | `D533 275B 0123 D0A6 79F5  1FF4 8F4F 9A45 D7FF EE74` | keys.openpgp.org; deckungsgleich mit `MAINTAINERS.md` im Release und dem GitHub-Konto des Maintainers |
| `util-linux` | Karel Zak | `B0C6 4D14 301C C6EF AEDF 60E4 E4B7 1D5E EC39 C284` | keyserver.ubuntu.com |

Eine solche Ausnahme muss im Rezept mit `unsigned = "<Grund>"` stehen; `bz verify`
listet sie jedes Mal auf. Sobald Upstream signiert, wird umgestellt.

## Schlüssel ändern

Ein neuer oder geänderter Schlüssel ist eine Vertrauensentscheidung und gehört
in einen eigenen Commit, mit Herkunft und Fingerprint in dieser Tabelle.
`bz` prüft alle Quellen eines Schlüsselbunds automatisch neu, sobald er sich ändert.
