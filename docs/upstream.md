# Upstream: eigene Fixes und Berichte

Was Beize an Upstream gibt, in welchem Stand. Die einzige Liste dafür; die Patch-READMEs
(`kernel/patches/`, `recipes/patches/`) sagen nur, was getragen wird und warum. Entwürfe liegen
in WSL `~/upstream/<name>/`; der Besitzer sendet (Kernel per b4: `~/upstream/EINREICHEN.md`;
systemd: Text schreibt er selbst, systemds KI-Regeln). Antworten landen in seinem Postfach –
lore.kernel.org und Patchwork sperren automatische Abrufe. Hier wird ersetzt, nicht angehängt.

Geprüft am **27.11.2026** gegen Linux 7.3.6 und Mainline (v7.4-rc4), systemd main 39c2bf5,
Smithay master 46643d2.

## Angenommen

| Was | Wo | Folge für Beize |
| --- | --- | --- |
| nouveau/gsp: nur an den Host geleitete GSP-Interrupts (`kernel/patches/0002`) | Mainline `54d3ff1b5ac0` (10.11., für 7.4), `Cc: stable` | Fällt mit dem ersten 7.3.x/7.4, der ihn hat (`bz` stoppt dann den Bau) |
| Smithay: importierte drm_syncobj-Timelines beim Drop freigeben | PR #2244, gemergt 29.10. | `[patch.crates-io]` auf unseren Fork fällt mit dem nächsten Smithay-Release (noch keins nach 0.7.0) |

## Gesendet, wartet

| Was | Wo, seit | Stand | Wir tragen es |
| --- | --- | --- | --- |
| nouveau/gsp: EDID-Blöcke 2/3 (MOT-Startbedingung) | Mail 21.10. (b4 `sent/20261021-nouveau-edid-segment-…`) | Nicht in Mainline/7.3.6 | `kernel/patches/0001` |
| mei: xHCI des 700er PCH-S nicht binden (5,5 s Start) | Mail 11.10. (`sent/20261011-mei-xhci-…`) | Nicht in char-misc (alle Zweige) | nein (#12) |
| USB: Strings ohne Gerätesperre (udev 1,7 s) | Mail 11.10., v2 (`sent/…usb-string-attrs…-v2`) | Nicht in usb (alle Zweige) | nein (#12) |
| systemd logind: Display neu wählen bei SetType/SetClass | PR [#44842](https://github.com/systemd/systemd/pull/44842), 16.11. | **Label `reviewed/needs-rework`**: Review-Bot, 3 Vorschläge + 1 Nit offen, unbeantwortet (unten) | `recipes/patches/systemd/0001` |
| ntpd-rs: Spawner wiederholt offline jede Sekunde | Issue [#2539](https://github.com/pendulum-project/ntpd-rs/issues/2539), 10.10. | offen, keine Antwort | `[cargo.update]` in `recipes/ntpd-rs.toml` bleibt |

**#44842 – was die Nacharbeit braucht** (Bot-Review zu a635fab653, `logind-session.c`):
1. Z. 1286: wird `s` als Display herabgestuft und gewinnt eine dritte Sitzung, muss auch die neue
   Display-Sitzung gespeichert werden (`IS_DISPLAY=1`), sonst stellt `session_load()` keins her.
2. Z. 1298: beim Abbau (`manager_free()` → `session_free()` → `session_drop_controller()`) keine
   Neuwahl, sonst stehen nach einem Neustart zwei `IS_DISPLAY=1`.
3. Z. 1310: Test fehlt – TEST-35: zweite Sitzung, `SetClass user` als root, dann
   `loginctl show-user -P Display` und die Statusdateien.
4. Nit Z. 1288: `session_save(s)` vor `user_save()` und dem Display-Signal.
Die roten Fedora-rawhide-rpm-Builds sind nicht mehr zu klären (Copr-Logs abgelaufen); andere
PRs vom selben Tag waren grün – der nächste Push zeigt es. v2 bereitet Claude in
`~/upstream/systemd-logind-display/` vor.

## Fertig, noch nicht gesendet

| Was | Entwurf | Vor dem Senden |
| --- | --- | --- |
| systemd: kein Timeout, solange polkit fragt (PID 1 und run0), für Issue [#44507](https://github.com/systemd/systemd/issues/44507) | `systemd-run0-timeout/` (wir tragen ihn: `recipes/patches/systemd/0002`) | Passt nicht mehr auf main (nur Kommentare `/* usec= */`); **main startet run0 über ptybrokerd**: dort wartet der Client per Varlink 45 s (`VARLINK_DEFAULT_TIMEOUT_USEC`) – der Patch braucht dafür einen Teil mehr. Claude setzt ihn neu auf |
| systemd veritysetup-generator: `usrhash=`-Gerät wartet 90 s nach Fehlschlag | `systemd-veritysetup-cmdline-device/` (Patch + Issue) | Passt auf main; #44880 (18.11.) hat den Generator geändert, Verhalten neu belegen |
| systemd gpt-auto: verschlüsseltes Root scheitert nach 90 s an der Passwortabfrage | `systemd-gpt-auto-timeout/ISSUE.md` | gegen main neu belegen |
| systemd 99-systemd.rules: verity-Gerät mit kaputten Daten wird nie „ready“ | `systemd-verity-not-ready/ISSUE.md` | gegen main neu belegen |
| systemd run0 verliert die letzte Ausgabe (`ptyfwd.c` `drained()`) | `systemd-ptyfwd-drain/ERKLAERUNG.md` | ptyfwd und run0 auf main stark umgebaut (ptybroker, 20.–23.11.): erst prüfen, ob es dort noch auftritt |
| sbsigntools: `sbvarsign` datiert Signaturen einen Monat zu früh | `sbsigntools-month-patch/` | an die Liste sbsigntools (James Bottomley) |
| Smithay: 4 Berichte (drm-master-Warnung unter logind, Mode-Blob 0, Cursor-Hotspot virtueller Treiber, Blob-Warnung auf abgeschaltetem CRTC) | `smithay/ISSUES.md` | Alle vier noch in master 46643d2; als Issues einreichen |
