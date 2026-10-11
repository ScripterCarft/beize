# Signaturschlüssel

Jede Quelle in `recipes/` muss von ihren Upstream-Maintainern signiert sein.
`bz` prüft die Signatur gegen genau einen Schlüsselbund aus diesem Verzeichnis:
OpenPGP (`<name>.gpg`, mit `gpgv`) oder – für Projekte, die mit Gits SSH-Signaturen
signieren – SSH (`<name>.allowed_signers`, mit `ssh-keygen -Y verify`). Jeder Bund
enthält nur die Schlüssel, die das Projekt tatsächlich signieren
(`gpg --export --export-options export-minimal`).

Prüfen aller Quellen: `./bz verify`

| Schlüsselbund | Signiert von | Fingerprint | Bezogen über |
| --- | --- | --- | --- |
| `acl` | Andreas Grünbacher (signiert attr und acl) | `259B 3792 B3D6 D319 212C C4DC D5BF 9FEB 0313 653A` | keyserver.ubuntu.com |
| `bash` | Chet Ramey | `7C01 35FB 088A AF6C 66C6 50B9 BB58 69F0 64EA 74AB` | GNU-Keyring |
| `binutils` | Nick Clifton | `3A24 BC1E 8FB4 09FA 9F14 3718 13FC EF89 DD9E 3C4F` | GNU-Keyring |
| `ca-certificates` | Weder Mozillas `certdata.txt` noch curls PEM-Auszug daraus sind signiert (geprüft 2026-10-09) | Datei per SHA-256 gepinnt, über HTTPS von curl.se, Prüfsumme deckungsgleich mit der veröffentlichten `.sha256` |
| `coreutils` | Pádraig Brady | `6C37 DC12 121A 5006 BC1D B804 DF6F D971 3060 37D9` | GNU-Keyring |
| `cryptsetup` | Milan Broz | `2A29 1824 3FDE 4664 8D06 86F9 D9B0 577B D93E 98FC` | kernel.org pgpkeys-Repository |
| `curl` | Daniel Stenberg | `27ED EAF2 2F3A BCEB 50DB 9A12 5CC9 08FD B71E 12C2` | keyserver.ubuntu.com; deckungsgleich mit `daniel.haxx.se/mykey.asc` |
| `dbus` | Simon McVittie | `7A07 3AD1 AE69 4FA2 5BFF 62E5 235C 099D 3EB3 3076` | keyserver.ubuntu.com |
| `duktape` | duktape signiert weder Archive noch Tags (geprüft 2026-10-11) | Archiv per SHA-256 gepinnt; dasselbe, aus dem Arch Linux baut |
| `e2fsprogs` | Theodore Ts'o | `2B69 B954 DBFE 0879 2881 37C9 F2F9 5956 950D 81A3` | keys.openpgp.org |
| `expat` | Sebastian Pipping | `CB8D E70A 90CF BF6C 3BF5 CC56 9626 2ACF FBD3 AEC6` | keyserver.ubuntu.com |
| `gcc` | Richard Biener | `7F74 F97C 1034 68EE 5D75 0B58 3AB0 0996 FC26 A641` | GNU-Keyring |
| `glib` | Philip Withnall (GNOME) | `923B 7025 EE03 C1C5 9F42 684C F094 2E89 4B2E AFA0` | keyserver.ubuntu.com; signierter Git-Tag (mit Git-EVTag), Unterschlüssel `6DFB 50D0 F26A E9F5 09B0 12CD C5C4 2CFB 2686 37CA`; das Submodul gvdb ist über den signierten Baum gepinnt |
| `glibc` | Andreas K. Hüttel | `35B1 7DF5 7525 77CA 0C54 1CEB 94BF DF44 84AD 142F` | keys.openpgp.org (GNU-Keyring enthält eine abgelaufene Fassung) |
| `gmp` | Niels Möller | `343C 2FF0 FBEE 5EC2 EDBE F399 F359 9FF8 28C6 7298` | GNU-Keyring |
| `intel-microcode` | Intel signiert weder Archive noch Tags oder Commits (geprüft 2026-10-10) | Archiv per SHA-256 gepinnt; jedes Microcode-Update trägt Intels Signatur, die die CPU vor dem Einspielen prüft |
| `iputils` | Petr Vorel (SUSE) | `2016 FEA4 858B 1C36 B32E 833A C0DE C2EE 72F3 3A5F` | keyserver.ubuntu.com; signiert die Release-Archive und die Tags auf GitHub |
| `json-c` | Upstream signiert weder Release-Archive noch Tags (geprüft 2026-10-07) | Commit des Release-Tags gepinnt; derselbe Tag, aus dem Arch Linux baut |
| `kbd` | Alexey Gladkov | `7F2A 3D07 2981 49A0 793C 9A4E A45A BA54 4CFF D434` | keyserver.ubuntu.com; deckungsgleich mit WKD kernel.org |
| `kmod` | Lucas De Marchi | `EAB3 3C96 9001 3C73 3916 AC83 9BA2 A5A6 30CB EA53` | keyserver.ubuntu.com |
| `libcap` | Andrew G. Morgan | `38A6 4469 8C69 7873 44E9 54CE 29EE 848A E2CC F3F4` | WKD kernel.org |
| `libffi` | libffi signiert weder Archive noch Tags (geprüft 2026-10-11) | Archiv per SHA-256 gepinnt; dasselbe, aus dem Arch Linux baut |
| `libseccomp` | Tom Hromatka (Oracle) | `47A6 8FCE 37C7 D702 4FD6 5E11 356C E62C 2B52 4099` | keyserver.ubuntu.com; Maintainer laut `SECURITY.md` des Projekts |
| `libxcrypt` | Björn Esser | `678C E3FE E430 3115 96DB 8C16 F52E 9800 7594 C21D` | keyserver.ubuntu.com; deckungsgleich mit `libxcrypt-gpgkey.asc` im Release |
| `libxkbcommon` | Upstream signiert weder Tags noch Commits und veröffentlicht keine signierten Archive (geprüft 2026-10-11) | Commit des Release-Tags gepinnt; derselbe Tag, aus dem Arch Linux baut |
| `linux` | Greg Kroah-Hartman | `647F 2865 4894 E3BD 4571 99BE 38DB BDC8 6092 693E` | WKD kernel.org |
| `linux-firmware` | Josh Boyer | `4CDE 8575 E547 BF83 5FE1 5807 A31B 6BD7 2486 CFD6` | keyserver.ubuntu.com |
| `linux-pam` | Dmitry V. Levin | `7BEC FE3A F7B2 80BB 52FF 77F1 04BA 4521 C996 DDE1` | keyserver.ubuntu.com; signiert mit Unterschlüssel `FB3D CD19 FF87 4735 E4F6 1CE6 4E9F E852 2F16 0EC2` |
| `lvm2` | Marian Csontos | `D501 A478 440A E2FD 130A 1BE8 B911 2431 E509 039F` | keys.openpgp.org |
| `mpc` | Andreas Enge | `AD17 A21E F8AE D8F1 CC02 DBD9 F7D5 C9BF 765C 61E3` | GNU-Keyring |
| `mpfr` | Vincent Lefèvre | `A534 BE3F 83E2 41D9 1828 0AEB 5831 D11A 0D4D B02A` | GNU-Keyring |
| `ntpd-rs` | David Venhoek (Tweede golf), SSH | `SHA256:xQQw30dT1YTuj8KC9IC7tJ9pGtcKOReK54ClMRIEABc` (RSA) | GitHub-Signaturschlüssel des Maintainers (seit 2023, Hardware-Token); signiert die Release-Commits, die Tags sind leichtgewichtig |
| `openssl` | OpenSSL | `C46E D3F2 CBEF DA1F DAAD A442 64ED 7B1D CCE7 1CB2` | openssl-library.org/source/pubkeys.asc |
| `pcre2` | Nicholas Wilson | `A955 3620 4A3B B489 7152 3128 2A98 E77E B6F2 4CA8` | keyserver.ubuntu.com; deckungsgleich mit `SECURITY.md` des Projekts; signiert mit Unterschlüssel `BACF 71F1 0404 D576 1C09 D392 021D E40B FB63 B406` |
| `polkit` | polkit signiert weder Tags (leichtgewichtig) noch Commits und veröffentlicht keine eigenen Archive (geprüft 2026-10-11) | Commit des Release-Tags gepinnt |
| `systemd` | Luca Boccassi | `AC24 AAC7 DDC4 20F1 8ECA 6B91 286B F7EF CD77 241E` | keyserver.ubuntu.com, signierter Git-Tag |
| `tpm2-tss` | Andreas Fuchs (Infineon) | `D533 275B 0123 D0A6 79F5  1FF4 8F4F 9A45 D7FF EE74` | keys.openpgp.org; deckungsgleich mit `MAINTAINERS.md` im Release und dem GitHub-Konto des Maintainers |
| `tzdata` | Paul Eggert | `7E37 92A9 D8AC F7D6 33BC 1588 ED97 E90E 62AA 7E34` | keys.openpgp.org; derselbe Schlüssel steht (abgelaufen) im GNU-Keyring |
| `util-linux` | Karel Zak | `B0C6 4D14 301C C6EF AEDF 60E4 E4B7 1D5E EC39 C284` | keyserver.ubuntu.com |
| `xkeyboard-config` | Sergey Udaltsov | `FFB4 CCD2 75AA A422 F5F9 808E 0661 D98F C933 A145` | keyserver.ubuntu.com |
| `xz` | Lasse Collin | `3690 C240 CE51 B467 0D30 AD1C 38EE 757D 6918 4620` | keyserver.ubuntu.com; deckungsgleich mit `tukaani.org/misc/lasse_collin_pubkey.txt` |
| `zlib` | Mark Adler | `5ED4 6A67 21D3 6558 7791 E2AA 783F CD8E 58BC AFBA` (DSA) | keyserver.ubuntu.com; signiert die Archive auf zlib.net |
| `zstd` | Zstandard Release Signing Key | `4EF4 AC63 455F C9F4 545D 9B7D EF8F E995 28B5 2FFD` (DSA) | keyserver.ubuntu.com; signiert die Release-Archive auf GitHub |

Eine solche Ausnahme muss im Rezept mit `unsigned = "<Grund>"` stehen; `bz verify`
listet sie jedes Mal auf. Sobald Upstream signiert, wird umgestellt.

## Schlüssel ändern

Ein neuer oder geänderter Schlüssel ist eine Vertrauensentscheidung und gehört
in einen eigenen Commit, mit Herkunft und Fingerprint in dieser Tabelle.
`bz` prüft alle Quellen eines Schlüsselbunds automatisch neu, sobald er sich ändert.
