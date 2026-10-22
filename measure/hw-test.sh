#!/bin/bash
# Measurement build only (measure/desktop), never on main: the hardware
# test of Beize's desktop (ADR 0027, step 3 g), run from the USB stick in
# Beize's terminal: the terminal's sample, Beize UI's sample, the frame
# schedule with the test client. Steps: docs/hardware.md.
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
    for p in beize-testclient beize-ui-test beize-terminal-sample; do
        cp "$here/$p" "$rt/$p" && chmod 700 "$rt/$p" || exit 1
    done

    clear
    "$rt/beize-terminal-sample"
    echo
    echo "  1/3 Terminal: die Probe des Terminals. Schrift scharf, Farben und Rahmen wie in"
    echo "  der Anleitung? Weiter mit Enter."
    read -r

    echo "  2/3 Beize UI: Gleich zeigt ein Fenster 30 Sekunden lang Text hell auf dunkel und"
    echo "  dunkel auf hell. Wirkt der helle Text kräftiger als der dunkle? Danach kommt das"
    echo "  Terminal von selbst zurück. Start mit Enter."
    read -r
    systemd-run --user --quiet --unit=hw-test-ui "$rt/beize-ui-test" --seconds 30 || exit 1
    wait_for hw-test-ui.service

    echo "  3/3 Bildtakt: Gleich füllt das Testfenster 20 Sekunden lang den Bildschirm. Dabei"
    echo "  a s d tippen, die Maus über das Fenster bewegen und einmal links klicken. Danach"
    echo "  kommt das Terminal von selbst zurück. Start mit Enter."
    read -r
    date '+%F %T' > "$out/client-start.txt"
    systemd-run --user --quiet --unit=hw-test-client \
        "$rt/beize-testclient" --frames 4800 --seconds 20 || exit 1
    wait_for hw-test-client.service
    # The compositor sums the frames up once they stopped for a quarter
    # second; it looks twice a second.
    sleep 2
fi

b="-b $boot"
journalctl --user $b -u beize-compositor.service -o short-monotonic > "$out/compositor.log" 2>&1
journalctl --user $b -u hw-test-client.service -o short-monotonic > "$out/client.log" 2>&1
journalctl --user $b -u hw-test-ui.service -o short-monotonic > "$out/ui.log" 2>&1
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
grep -h -E 'GPU |screen |renderer on|rendering with|cursor plane|lavapipe|keyboard layout' \
    "$out/compositor.log" | sed 's/^.*\]: //' | grep -v '^pacing: ' | sort -u
grep -h -E 'frames:|drawing:|configure|keymap' "$out/client.log" | sed 's/^.*\]: //'
if [ -s "$out/client-start.txt" ]; then
    # The terminal's frames have summaries of their own (the Enter just
    # pressed): the client's is the one with the most frames.
    echo "Bildtakt des Testfensters, vom Compositor selbst gemessen:"
    journalctl --user -u beize-compositor.service --since "$(cat "$out/client-start.txt")" \
        -o cat | grep '^pacing: ' | sort -t ' ' -k 2 -n | tail -n 1
fi
echo "Tasten im Fenster: $(grep -c ' key ' "$out/client.log"), Klicks: $(grep -c 'button ' "$out/client.log")"
echo "Warnungen des Compositors:"
journalctl --user $b -u beize-compositor.service -p warning -o cat 2>/dev/null | sort | uniq -c
sync
