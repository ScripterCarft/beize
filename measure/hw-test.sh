#!/bin/bash
# Measurement build only (measure/desktop), never on main: the hardware
# test of Beize's desktop (ADR 0027, step 7), run from the USB stick on tty2
# while beize-session runs on tty1. Steps: docs/hardware.md.
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

if [ "$mode" = test ]; then
    if ! systemctl --user -q is-active beize-compositor.service; then
        echo "Der Desktop läuft nicht: zuerst auf tty1 anmelden und beize-session eingeben."
        exit 1
    fi
    # The client finds the compositor through the service manager, as an app
    # will: 20 s at up to 240 frames a second, with the bar moving through
    # the top, each frame on the last one's "frame done". It runs from the
    # runtime directory, whatever the stick's file system says of exec.
    cp "$here/beize-testclient" "$XDG_RUNTIME_DIR/beize-testclient" &&
        chmod 700 "$XDG_RUNTIME_DIR/beize-testclient" || exit 1
    # A timer fires up to a minute late unless told otherwise (AccuracySec).
    systemd-run --user --quiet --unit=hw-test-client --on-active=10 \
        --timer-property=AccuracySec=100ms \
        "$XDG_RUNTIME_DIR/beize-testclient" --frames 4800 --seconds 20 || exit 1
    echo
    echo "  In 10 Sekunden erscheint das Testfenster für 20 Sekunden."
    echo "  JETZT Strg+Alt+F1 drücken. Im Fenster: ein paar Tasten tippen (a s d),"
    echo "  die Maus über das Fenster bewegen und einmal links klicken."
    echo "  Wenn das Fenster weg ist: Strg+Alt+F2, hier geht es weiter."
    echo
    # Until the timer has started the client and the client has ended.
    sleep 5
    while systemctl --user -q is-active hw-test-client.timer hw-test-client.service; do
        sleep 1
    done
fi

b="-b $boot"
journalctl --user $b -u beize-compositor.service -o short-monotonic > "$out/compositor.log" 2>&1
journalctl --user $b -u hw-test-client.service -o short-monotonic > "$out/client.log" 2>&1
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
    "$out/compositor.log" | sed 's/^.*\]: //' | sort -u
grep -h -E 'frames:|configure|keymap' "$out/client.log" | sed 's/^.*\]: //'
echo "Tasten im Fenster: $(grep -c ' key ' "$out/client.log"), Klicks: $(grep -c 'button ' "$out/client.log")"
echo "Warnungen des Compositors:"
journalctl --user $b -u beize-compositor.service -p warning -o cat 2>/dev/null | sort | uniq -c
sync
