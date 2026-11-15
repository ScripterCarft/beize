# 0030 – Fokus: wer mit der Tastatur nach vorne kommt

Status: Angenommen · 2026-11-15 (mit dem Besitzer besprochen)

## Kontext

Bis 0.6 kam ein neues Fenster nur in Ausnahmefällen mit der Tastatur nach vorne:
- kein Fenster war aktiv,
- es kam mit einem Token,
- es gehörte zur aktiven App (cgroup),
- die letzte Eingabe ging an Beize (Zähler „erwartet“),
- oder seine Unit stand in `~/.config/beize/come-forward`.

Der Besitzer lehnte die Liste und den Zähler ab (1.11.): Es wird nichts fest eingetragen und
nichts geraten. Sein Prüfstein: `op` im Terminal, und das Entsperrfenster von 1Password steht
vorne, mit dem Cursor im Passwortfeld, wie unter Windows und macOS.

Was 1Password unter Linux macht, ist am Paket 8.12.44 belegt (statisch untersucht, nicht
ausgeführt):
- Das Entsperrfenster („AuthPrompt“) ist bei jedem Anzeigen ein **neues** Electron-Fenster
  (`alwaysOnTop`, `skipTaskbar`, rahmenlos). Es fordert den Fokus nicht an und übergibt kein
  Token.
- Chromium holt ein Fenster nur über ein Token nach vorne, das es sich selbst ausstellt, mit der
  letzten Eingabe **in die eigene App** (`WaylandToplevelWindow::Activate`). Nach einer Eingabe
  im Terminal ist dieses Token ungültig.
- `op` spricht über den Socket `1Password-BrowserSupport.sock` direkt mit der App. `op` ist
  setgid `onepassword-cli` und damit für andere Prozesse gesperrt (nicht dumpbar). Ohne
  Root-Rechte lässt sich nicht feststellen, mit wem `op` verbunden ist.

Unter Windows (`AllowSetForegroundWindow`) und macOS (`yieldActivation`) gibt die aufrufende
Seite den Vortritt ausdrücklich ab. Unter Linux gibt es dafür nichts, und 1Password tut es dort
auch nicht.

Vorbilder: **Mutter** gibt einem neuen Fenster den Fokus, außer der Benutzer hat nach dessen
Zeitstempel im aktiven Fenster gearbeitet (`intervening_user_event_occurred`,
`focus-new-windows=smart`). Ein Wayland-Fenster ohne Token hat keinen Zeitstempel, es kommt
also nach vorne. **KWin** hält ab Werk dasselbe (`FocusStealingPreventionLevel` Low). Ein schon
vorhandenes Fenster holen beide nur mit einem gültigen Token nach vorne. Darauf ist 1Password
gebaut.

## Entscheidung

1. **Ein neues Fenster kommt mit der Tastatur nach vorne**, wie bei Mutter (`forward.rs`).
2. **Ein vorhandenes Fenster braucht ein gültiges Token** (xdg-activation, `activation.rs`,
   unverändert):
   - ausgestellt von der aktiven App,
   - mit einer Eingabe, die nicht älter ist als ihr Fokus,
   - nur einmal einlösbar.

   Dasselbe gilt für den Wunsch nach Vollbild. Ohne Token bleibt das Fenster, wo es ist.
3. **Vorne hat immer die Tastatur.** Hält Beize selbst die Tastatur (Menü, Alt+Tab, ein
   Overlay der Shell), öffnet ein neues Fenster hinter dem aktiven, ohne Tastatur, und der
   Compositor meldet das. Ein Fenster steht also nie vorne, während die Tasten an ein anderes
   gehen.
4. **Ersatzlos entfallen:** `come-forward`, der Zähler „erwartet“ und die cgroup der Clients
   (gebraucht wird sie nur noch, um die Shell zu erkennen).

## Folgen

- `op`, `git push` mit dem SSH-Agenten, die Browser-Erweiterung und die Schnellsuche von
  1Password zeigen ihr Fenster vorne mit der Tastatur. Das ist abgeleitet: Jedes davon ist ein
  neues Fenster. Der Smoke prüft den Mechanismus mit dem Testclient, die echten Fälle prüft
  der Besitzer auf Hardware (1Password ist für die Automatisierung tabu).
- Wie unter GNOME und KDE kann eine App im Hintergrund mit einem neuen Fenster die Tastatur
  übernehmen. Der Besitzer nimmt das bewusst in Kauf (15.11.). Den Schutz gegen ein vorderes
  Fenster ohne Tastatur gibt es weiter.
- Schutz für Passwortfelder (wie Secure Event Input bei macOS): erst mit dem
  Eingabeprotokoll (text-input), das Beize noch nicht hat.
- Systemauthentifizierung (polkit) bekommt einen eigenen Dialog der Shell, eigenes ADR.
