#!/bin/bash
# Measurement build only (measure/desktop), never on main: the hardware
# test of Beize's desktop (ADR 0027, step 3 g), run from the USB stick in
# Beize's terminal: the terminal's sample, Beize UI's sample, the frame
# schedule with the test client, the load test's scene (ADR 0028 §9), a
# window resized by hand and typed in (what the compositor holds, the
# prompt, a line longer than the window, how soon keys show) and the
# screen going off when idle and coming back (ADR 0029 §7: does the
# monitor get its picture again after the CRTC was off?). The monitor
# switched off and on by hand is not tested any more: it stays black with
# Fedora too, below Beize (CLAUDE.md §11).
# Steps: docs/hardware.md.
#
#   bash /run/stick/beize-test/hw-test.sh        the test, then the logs
#   bash /run/stick/beize-test/hw-test.sh logs   only the logs of this start
#   bash /run/stick/beize-test/hw-test.sh -1     only the logs of the start
#                                                before (after a frozen screen)
#
# Everything lands in results-<time>/ next to this script, on the stick.
set -u
here=$(dirname "$(readlink -f "$0")")
mode=${1:-test}
case $mode in
    test | logs) boot=0 ;;
    -1) boot=-1 ;;
    *) echo "hw-test.sh [logs | -1]"; exit 1 ;;
esac
out=$here/results-$(date +%Y%m%d-%H%M%S)
if ! mkdir -p "$out" 2>/dev/null; then
    echo "Der Stick ist nicht beschreibbar: mit uid=\$(id -u) einhängen (Schritt 6 der Anleitung)."
    exit 1
fi

# Waits until the user unit $1 has ended.
wait_for() {
    sleep 1
    while systemctl --user -q is-active "$1"; do sleep 1; done
}

if [ "$mode" = test ]; then
    if ! systemctl --user -q is-active beize-compositor.service; then
        echo "Der Desktop läuft nicht: hw-test.sh gehört ins Terminal des Desktops."
        exit 1
    fi
    # The programs run from the runtime directory, whatever the stick's
    # file system says of exec; the windows through the service manager,
    # as an app's would.
    rt=$XDG_RUNTIME_DIR
    for p in beize-testclient beize-ui-test beize-load-test beize-terminal-sample; do
        cp "$here/$p" "$rt/$p" && chmod 700 "$rt/$p" || exit 1
    done

    clear
    "$rt/beize-terminal-sample"
    echo
    echo "  1/6 Terminal: die Probe des Terminals. Schrift scharf, Farben und Rahmen wie in"
    echo "  der Anleitung? Weiter mit Enter."
    read -r

    echo "  2/6 Beize UI: Gleich zeigt ein Fenster 30 Sekunden lang Text hell auf dunkel und"
    echo "  dunkel auf hell. Wirkt der helle Text kräftiger als der dunkle? Danach kommt das"
    echo "  Terminal von selbst zurück. Start mit Enter."
    read -r
    systemd-run --user --quiet --unit=hw-test-ui "$rt/beize-ui-test" --seconds 30 || exit 1
    wait_for hw-test-ui.service

    echo "  3/6 Bildtakt: Gleich zeigt das Testfenster 20 Sekunden lang rot, grün, blau und"
    echo "  weiß, oben läuft ein dunkler Balken. Dabei a s d tippen, die Maus über das Fenster"
    echo "  bewegen und einmal links klicken. Danach schließt es sich von selbst. Start mit Enter."
    read -r
    date '+%F %T' > "$out/client-start.txt"
    systemd-run --user --quiet --unit=hw-test-client \
        "$rt/beize-testclient" --frames 4800 --seconds 20 || exit 1
    wait_for hw-test-client.service

    echo "  4/6 Last: Gleich öffnen sich 12 Fenster mit laufenden Farbverläufen, 40 Sekunden"
    echo "  lang. Zieh gleich zu Beginn das vorderste an seiner Titelleiste nach unten, bis es"
    echo "  zur Hälfte hinter dem Dock liegt, dann die Maus nicht mehr bewegen. Läuft alles"
    echo "  flüssig? Scheint das Fenster bewegt durchs Dock? Danach schließen sie sich von selbst."
    echo "  Start mit Enter."
    read -r
    date '+%F %T' > "$out/load-start.txt"
    systemd-run --user --quiet --unit=hw-test-load \
        "$rt/beize-load-test" --windows 12 --seconds 40 || exit 1
    wait_for hw-test-load.service

    echo "  5/6 Fenstergröße und Tasten: Öffne mit Strg+Alt+T ein zweites Terminal und zieh"
    echo "  es an der rechten unteren Ecke 20 Sekunden lang schnell immer wieder ganz groß und"
    echo "  klein. Folgt es der Maus und steht es beim Loslassen sofort still? Bleibt der"
    echo "  Prompt eine saubere Zeile? Dann darin ls, pwd, date und eine Zeile länger als das"
    echo "  Fenster tippen (echo und viele x): bricht sie um, statt mit < nach links zu"
    echo "  schieben? Mit Pfeil hoch und runter durchblättern: kommt jede Taste sofort, bleibt"
    echo "  nach der langen Zeile kein Rest stehen? Danach darin exit, dann hier Enter."
    date '+%F %T' > "$out/resize-start.txt"
    read -r

    echo "  6/6 Bildschirm aus: Nach Enter Maus und Tastatur nicht mehr anfassen. Nach 10"
    echo "  Minuten wird das Bild 10 Sekunden lang dunkler, dann geht der Monitor aus (Standby)."
    echo "  Warte danach noch etwa eine Minute, dann die Maus bewegen: Kommt das Bild in ein"
    echo "  paar Sekunden wieder, wie es war? Das Skript wartet selbst darauf (höchstens 20"
    echo "  Minuten) und schreibt die Zeiten auf. Bleibt es schwarz: erst Strg+Alt+F2, dann"
    echo "  Strg+Alt+F1; hilft das nicht, Reset und danach hw-test.sh -1. Start mit Enter."
    read -r
    off_start=$(date '+%F %T')
    echo "$off_start" > "$out/off-start.txt"
    # The compositor says each step (src/idle.rs); looked for every 2 s,
    # which is no input.
    said() {
        journalctl --user -u beize-compositor.service --since "$off_start" -o cat |
            grep -c -x "$1"
    }
    for _ in $(seq 600); do
        [ "$(said 'input: the screen is on again')" -ge 1 ] && break
        sleep 2
    done
    # The connector as the kernel has it once the picture is back.
    sleep 3
    grep -H . /sys/class/drm/card*-*/dpms /sys/class/drm/card*-*/status \
        > "$out/off-dpms.txt" 2>&1
    echo "  Bild wieder da? Dann hier Enter."
    read -r

    # The compositor sums the frames up once they stopped for a quarter
    # second; it looks twice a second.
    sleep 2
fi

# Every screenshot of this session (Super+Shift+3, whenever something
# looks wrong): they live in the runtime directory (src/screenshot.rs),
# gone with the session.
cp "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"/screenshots/*.png "$out/" 2>/dev/null

b="-b $boot"
journalctl --user $b -u beize-compositor.service -o short-monotonic > "$out/compositor.log" 2>&1
journalctl --user $b -u hw-test-client.service -o short-monotonic > "$out/client.log" 2>&1
journalctl --user $b -u hw-test-ui.service -o short-monotonic > "$out/ui.log" 2>&1
journalctl --user $b -u hw-test-load.service -o short-monotonic > "$out/load.log" 2>&1
journalctl --user $b -o short-monotonic > "$out/user-journal.log" 2>&1
cat /sys/class/drm/card*-*/modes > "$out/modes.txt" 2>&1
for edid in /sys/class/drm/card*-*/edid; do
    [ -s "$edid" ] && cp "$edid" "$out/edid-$(basename "$(dirname "$edid")").bin"
done
(. /usr/lib/os-release; echo "$IMAGE_VERSION $VARIANT_ID") > "$out/version.txt"
echo "  Für das Journal des Kernels fragt run0 nach deinem Passwort:"
run0 journalctl $b -o short-monotonic > "$out/system-journal.log" 2>&1

echo
echo "== Ergebnis (alles in $out)"
cat "$out/version.txt"
grep -h -E 'GPU |screen |desktop shown|renderer on|rendering with|cursor plane|lavapipe|keyboard layout' \
    "$out/compositor.log" | sed 's/^.*\]: //' | grep -v '^pacing: ' | sort -u
grep -h -E 'frames:|drawing:|configure|keymap' "$out/client.log" | sed 's/^.*\]: //'
if [ -s "$out/client-start.txt" ]; then
    # The terminal's frames have summaries of their own (the Enter just
    # pressed): the client's is the one with the most frames.
    echo "Bildtakt des Testfensters, vom Compositor selbst gemessen:"
    until=()
    [ -s "$out/load-start.txt" ] && until=(--until "$(cat "$out/load-start.txt")")
    journalctl --user -u beize-compositor.service --since "$(cat "$out/client-start.txt")" \
        "${until[@]}" -o cat | grep '^pacing: ' | sort -t ' ' -k 2 -n | tail -n 1
fi
if [ -s "$out/load-start.txt" ]; then
    # The drag at the start is a run of its own; the scene's 40 s come in
    # pieces of 30 s.
    echo "Lasttest, vom Compositor selbst gemessen:"
    until=()
    [ -s "$out/resize-start.txt" ] && until=(--until "$(cat "$out/resize-start.txt")")
    journalctl --user -u beize-compositor.service --since "$(cat "$out/load-start.txt")" \
        "${until[@]}" -o cat | grep '^pacing: ' | sort -t ' ' -k 2 -n | tail -n 2
    grep -h ': load: ' "$out/load.log" | sed 's/^.*\]: //'
fi
if [ -s "$out/resize-start.txt" ]; then
    # Logged when things are still, if it changed: flat after the resize
    # unless something leaks.
    echo "Größe nach dem Loslassen, vom Compositor:"
    journalctl --user -u beize-compositor.service --since "$(cat "$out/resize-start.txt")" \
        -o cat | grep '^resized ' | tail -n 5
    echo "Puffer des Compositors, vor und nach dem Ziehen:"
    journalctl --user -u beize-compositor.service --until "$(cat "$out/resize-start.txt")" \
        -o cat | grep '^held: ' | tail -n 1
    journalctl --user -u beize-compositor.service --since "$(cat "$out/resize-start.txt")" \
        -o cat | grep '^held: ' | tail -n 3
fi
if [ -s "$out/off-start.txt" ]; then
    echo "Bildschirm aus, vom Compositor (ab $(cat "$out/off-start.txt")):"
    journalctl --user -u beize-compositor.service --since "$(cat "$out/off-start.txt")" \
        -o short-monotonic | grep -E 'no input for|the screen is off|input: |screen |Setting new mode' |
        sed 's/^\[ *\([0-9.]*\)\].*\]: /\1 /'
    cat "$out/off-dpms.txt" 2>/dev/null
fi
echo "Tasten im Fenster: $(grep -c ' key ' "$out/client.log"), Klicks: $(grep -c 'button ' "$out/client.log")"
echo "Warnungen des Compositors:"
journalctl --user $b -u beize-compositor.service -p warning -o cat 2>/dev/null | sort | uniq -c
sync
