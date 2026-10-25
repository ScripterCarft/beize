#!/bin/bash
# Measurement build only (measure/desktop), never on main: the hardware
# test of Beize's desktop (ADR 0027, step 3 g), run from the USB stick in
# Beize's terminal: the terminal's sample, Beize UI's sample, the frame
# schedule with the test client, the load test's scene (ADR 0028 §9), a
# window resized by hand (what the compositor holds), screenshots.
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

    echo "  5/6 Fenstergröße: Öffne mit Strg+Alt+T ein zweites Terminal und zieh es an der"
    echo "  rechten unteren Ecke 20 Sekunden lang immer wieder ganz groß und klein, wie beim"
    echo "  Absturz in 0.5.103. Bleibt es offen? Danach darin exit, dann hier Enter."
    date '+%F %T' > "$out/resize-start.txt"
    read -r

    echo "  6/6 Bildschirmfotos: Mach mit Super+Shift+3 (oder der Druck-Taste) Fotos von allem,"
    echo "  was nicht schön aussieht: Hintergrund, Menüleiste, Dock, Titelleiste mit Knöpfen."
    echo "  Schieb dieses Terminal dafür ruhig beiseite. So viele du willst; fertig: Enter."
    read -r
    # The compositor writes them below the runtime directory (its sandbox).
    shots=$XDG_RUNTIME_DIR/screenshots
    if compgen -G "$shots/*.png" > /dev/null; then
        mkdir -p "$out/screenshots" && cp "$shots"/*.png "$out/screenshots/"
    fi
    # The compositor sums the frames up once they stopped for a quarter
    # second; it looks twice a second.
    sleep 2
fi

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
    echo "Puffer des Compositors, vor und nach dem Ziehen:"
    journalctl --user -u beize-compositor.service --until "$(cat "$out/resize-start.txt")" \
        -o cat | grep '^held: ' | tail -n 1
    journalctl --user -u beize-compositor.service --since "$(cat "$out/resize-start.txt")" \
        -o cat | grep '^held: ' | tail -n 3
fi
[ "$mode" = test ] && echo "Bildschirmfotos: $(find "$out/screenshots" -name '*.png' 2>/dev/null | wc -l)"
echo "Tasten im Fenster: $(grep -c ' key ' "$out/client.log"), Klicks: $(grep -c 'button ' "$out/client.log")"
echo "Warnungen des Compositors:"
journalctl --user $b -u beize-compositor.service -p warning -o cat 2>/dev/null | sort | uniq -c
sync
