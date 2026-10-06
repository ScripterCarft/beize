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
| `dbus` | Simon McVittie | `7A07 3AD1 AE69 4FA2 5BFF 62E5 235C 099D 3EB3 3076` | keyserver.ubuntu.com |
| `expat` | Sebastian Pipping | `CB8D E70A 90CF BF6C 3BF5 CC56 9626 2ACF FBD3 AEC6` | keyserver.ubuntu.com |
| `gcc` | Richard Biener | `7F74 F97C 1034 68EE 5D75 0B58 3AB0 0996 FC26 A641` | GNU-Keyring |
| `glibc` | Andreas K. Hüttel | `35B1 7DF5 7525 77CA 0C54 1CEB 94BF DF44 84AD 142F` | keys.openpgp.org (GNU-Keyring enthält eine abgelaufene Fassung) |
| `gmp` | Niels Möller | `343C 2FF0 FBEE 5EC2 EDBE F399 F359 9FF8 28C6 7298` | GNU-Keyring |
| `libcap` | Andrew G. Morgan | `38A6 4469 8C69 7873 44E9 54CE 29EE 848A E2CC F3F4` | WKD kernel.org |
| `linux` | Greg Kroah-Hartman | `647F 2865 4894 E3BD 4571 99BE 38DB BDC8 6092 693E` | WKD kernel.org |
| `mpc` | Andreas Enge | `AD17 A21E F8AE D8F1 CC02 DBD9 F7D5 C9BF 765C 61E3` | GNU-Keyring |
| `mpfr` | Vincent Lefèvre | `A534 BE3F 83E2 41D9 1828 0AEB 5831 D11A 0D4D B02A` | GNU-Keyring |
| `systemd` | Luca Boccassi | `AC24 AAC7 DDC4 20F1 8ECA 6B91 286B F7EF CD77 241E` | keyserver.ubuntu.com, signierter Git-Tag |
| `util-linux` | Karel Zak | `B0C6 4D14 301C C6EF AEDF 60E4 E4B7 1D5E EC39 C284` | keyserver.ubuntu.com |

Abgeglichen am 2026-10-06: Jeder Fingerprint ist der Aussteller der Signatur auf
dem gepinnten Release, und die Signatur ist gültig.

## Schlüssel ändern

Ein neuer oder geänderter Schlüssel ist eine Vertrauensentscheidung und gehört
in einen eigenen Commit, mit Herkunft und Fingerprint in dieser Tabelle.
`bz` prüft alle Quellen eines Schlüsselbunds automatisch neu, sobald er sich ändert.
