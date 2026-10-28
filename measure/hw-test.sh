#!/bin/bash
# Measurement build only (measure/desktop), never on main: the hardware
# test of Beize's desktop (ADR 0027, step 3 g), run from the USB stick in
# Beize's terminal: the terminal's sample, the shell's Beize menu and
# control center (ADR 0028 e: popups, grabs, glass, light/dark, what logind
# lets the menu do), Beize UI's sample, the frame
# schedule with the test client, explicit sync on nouveau's syncobjs
# (ADR 0027 §7), the load test's scene (ADR 0028 §9) and a window resized
# by hand and typed in (what the compositor holds, the prompt, a line
# longer than the window, how soon keys show). The screen going off when
# idle is not tested any more: 0.5.111 showed the monitor comes back
# (CLAUDE.md §11), and the monitor switched off and on by hand stays black
# with Fedora too, below Beize.
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

    echo "  2/6 Menü und Kontrollzentrum (nichts ausschalten oder abmelden):"
    echo "  a) Klick auf das Beize-Zeichen oben links. Das Menü: ruhig, fast weiß, dünner"
    echo "     heller Rand, Schatten nur außen? Foto mit Super+Shift+3, dann Esc."
    echo "  b) Öffne es wieder und klick daneben auf den Schreibtisch: geht es zu?"
    echo "  c) Tippe hier abc: erscheint es? (Die Tastatur ist zurück beim Terminal.)"
    echo "  d) Klick oben rechts auf den Schalter: Kacheln kleiner, rechts bündig mit dem Ende"
    echo "     der Uhr, unter dem Schalter eine runde Pille mit Luft zu Globus und Uhr? Foto,"
    echo "     dann auf den Kreis: wird alles dunkel? Foto, noch einmal auf den Kreis (zurück"
    echo "     auf hell), Esc."
    echo "  e) Öffne das Beize-Menü, dann zieh dieses Fenster an seiner Titelleiste: geht das"
    echo "     Menü zu und wandert das Fenster gleich mit? Ebenso mit dem Kontrollzentrum."
    echo "  f) Öffne das Kontrollzentrum, schließ es mit Esc und klick sofort auf das"
    echo "     Beize-Zeichen: geht das Menü beim ersten Klick auf?"
    echo "  Danach hier Enter."
    date '+%F %T' > "$out/shell-start.txt"
    read -r

    echo "  3/6 Beize UI: Gleich zeigt ein Fenster 30 Sekunden lang Text hell auf dunkel und"
    echo "  dunkel auf hell. Wirkt der helle Text kräftiger als der dunkle? Danach kommt das"
    echo "  Terminal von selbst zurück. Start mit Enter."
    read -r
    systemd-run --user --quiet --unit=hw-test-ui "$rt/beize-ui-test" --seconds 30 || exit 1
    wait_for hw-test-ui.service

    echo "  4/6 Bildtakt: Gleich zeigt das Testfenster 20 Sekunden lang rot, grün, blau und"
    echo "  weiß, oben läuft ein dunkler Balken. Dabei a s d tippen, die Maus über das Fenster"
    echo "  bewegen und einmal links klicken. Danach schließt es sich von selbst, drei kurze"
    echo "  Testfenster folgen von allein, das letzte schwarz (nichts tun). Sie zeichnen eine"
    echo "  feste Zahl Bilder und bleiben danach stehen, bis ihre Zeit um ist: so gewollt."
    echo "  Start mit Enter."
    read -r
    date '+%F %T' > "$out/client-start.txt"
    systemd-run --user --quiet --unit=hw-test-client \
        "$rt/beize-testclient" --frames 4800 --seconds 20 || exit 1
    wait_for hw-test-client.service
    # wp_fifo and wp_commit_timing as Mesa's Vulkan FIFO and video players
    # use them: how many frames show at the very next vblank, which the
    # VM's smoke test cannot tell (CLAUDE.md §11). No input needed.
    systemd-run --user --quiet --unit=hw-test-fifo \
        "$rt/beize-testclient" --fifo 2400 --seconds 20 || exit 1
    wait_for hw-test-fifo.service
    systemd-run --user --quiet --unit=hw-test-timed \
        "$rt/beize-testclient" --timed 240 --seconds 20 || exit 1
    wait_for hw-test-timed.service
    # linux-drm-syncobj-v1 with the timelines of nouveau, not virtio-gpu's.
    systemd-run --user --quiet --unit=hw-test-syncobj \
        "$rt/beize-testclient" --syncobj || exit 1
    wait_for hw-test-syncobj.service

    echo "  5/6 Last: Gleich öffnen sich 12 Fenster mit laufenden Farbverläufen, 40 Sekunden"
    echo "  lang. Zieh gleich zu Beginn das vorderste an seiner Titelleiste nach unten, bis es"
    echo "  zur Hälfte hinter dem Dock liegt, dann die Maus nicht mehr bewegen. Läuft alles"
    echo "  flüssig? Scheint das Fenster bewegt durchs Dock? Danach schließen sie sich und öffnen"
    echo "  sich noch einmal 30 Sekunden lang von selbst (nichts tun). Start mit Enter."
    read -r
    date '+%F %T' > "$out/load-start.txt"
    systemd-run --user --quiet --unit=hw-test-load \
        "$rt/beize-load-test" --windows 12 --seconds 40 || exit 1
    wait_for hw-test-load.service
    date '+%F %T' > "$out/rotate-start.txt"
    systemd-run --user --quiet --unit=hw-test-rotate \
        "$rt/beize-load-test" --windows 12 --seconds 30 --rotate || exit 1
    wait_for hw-test-rotate.service

    echo "  6/6 Fenstergröße und Tasten: Öffne mit Strg+Alt+T ein zweites Terminal und zieh"
    echo "  es an der rechten unteren Ecke 20 Sekunden lang schnell immer wieder ganz groß und"
    echo "  klein. Folgt es der Maus und steht es beim Loslassen sofort still? Bleibt der"
    echo "  Prompt eine saubere Zeile? Dann darin ls, pwd, date und eine Zeile länger als das"
    echo "  Fenster tippen (echo und viele x): bricht sie um, statt mit < nach links zu"
    echo "  schieben? Mit Pfeil hoch und runter durchblättern: kommt jede Taste sofort, bleibt"
    echo "  nach der langen Zeile kein Rest stehen? Danach darin exit, dann hier Enter."
    date '+%F %T' > "$out/resize-start.txt"
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
journalctl --user $b -u beize-shell.service -o short-monotonic > "$out/shell.log" 2>&1
loginctl list-sessions --no-legend > "$out/sessions.txt" 2>&1
loginctl show-user "$(id -u)" -p Display >> "$out/sessions.txt" 2>&1
journalctl --user $b -u hw-test-client.service -o short-monotonic > "$out/client.log" 2>&1
journalctl --user $b -u hw-test-ui.service -o short-monotonic > "$out/ui.log" 2>&1
journalctl --user $b -u hw-test-load.service -o short-monotonic > "$out/load.log" 2>&1
journalctl --user $b -u hw-test-fifo.service -o short-monotonic > "$out/fifo.log" 2>&1
journalctl --user $b -u hw-test-timed.service -o short-monotonic > "$out/timed.log" 2>&1
journalctl --user $b -u hw-test-rotate.service -o short-monotonic > "$out/rotate.log" 2>&1
journalctl --user $b -u hw-test-syncobj.service -o short-monotonic > "$out/syncobj.log" 2>&1
journalctl --user $b -o short-monotonic > "$out/user-journal.log" 2>&1
cat /sys/class/drm/card*-*/modes > "$out/modes.txt" 2>&1
for edid in /sys/class/drm/card*-*/edid; do
    [ -s "$edid" ] && cp "$edid" "$out/edid-$(basename "$(dirname "$edid")").bin"
done
(. /usr/lib/os-release; echo "$IMAGE_VERSION $VARIANT_ID") > "$out/version.txt"
echo "  Für das Journal des Kernels fragt run0 nach deinem Passwort:"
# run0 keeps the terminal only while its own output goes there: the file
# is written inside (0.5.112 said "Access denied" with it redirected).
run0 bash -c 'journalctl "$1" -o short-monotonic > "$0" 2>&1' \
    "$out/system-journal.log" "--boot=$boot"

echo
echo "== Ergebnis (alles in $out)"
cat "$out/version.txt"
grep -h -E 'GPU |screen |desktop shown|renderer on|rendering with|cursor plane|lavapipe|keyboard layout' \
    "$out/compositor.log" | sed 's/^.*\]: //' | grep -v '^pacing: ' | sort -u
grep -h -E 'frames:|drawing:|configure|keymap' "$out/client.log" | sed 's/^.*\]: //'
# The shell's part (2/6): logind's answer ("yes" three times with only
# tty1 signed in), the menus opened and closed, the appearance switched.
echo "Menü und Kontrollzentrum:"
grep -h -E 'Beize menu may|network: ' "$out/shell.log" | sed 's/^.*\]: //' | tail -n 3
grep -h -o -E '(BeizeMenu|ControlCenter) (open|closed)' "$out/shell.log" | sort | uniq -c
grep -h -E 'appearance (Light|Dark), chosen|a popup' "$out/compositor.log" | sed 's/^.*\]: //'
sed 's/^/  /' "$out/sessions.txt"
if [ -s "$out/client-start.txt" ]; then
    # The terminal's frames have summaries of their own (the Enter just
    # pressed): the client's is the one with the most frames.
    echo "Bildtakt des Testfensters, vom Compositor selbst gemessen:"
    until=()
    [ -s "$out/load-start.txt" ] && until=(--until "$(cat "$out/load-start.txt")")
    journalctl --user -u beize-compositor.service --since "$(cat "$out/client-start.txt")" \
        "${until[@]}" -o cat | grep '^pacing: ' | sort -t ' ' -k 2 -n | tail -n 1
    echo "wp_fifo und wp_commit_timing:"
    grep -h -E ': (presentation|fifo|timed): ' "$out/fifo.log" "$out/timed.log" | sed 's/^.*\]: //'
    echo "Explizite Sync (linux-drm-syncobj-v1):"
    grep -h -E ': (syncobj [a-z]+|beize-testclient): ' "$out/syncobj.log" | sed 's/^.*\]: //'
fi
if [ -s "$out/load-start.txt" ]; then
    # The drag at the start is a run of its own; the scene's 40 s come in
    # pieces of 30 s.
    echo "Lasttest, vom Compositor selbst gemessen:"
    until=()
    [ -s "$out/rotate-start.txt" ] && until=(--until "$(cat "$out/rotate-start.txt")")
    journalctl --user -u beize-compositor.service --since "$(cat "$out/load-start.txt")" \
        "${until[@]}" -o cat | grep '^pacing: ' | sort -t ' ' -k 2 -n | tail -n 2
    grep -h -E ': (load|window [0-9]+): ' "$out/load.log" | sed 's/^.*\]: //'
    # Beize UI draws with NVK: its buffers come with timelines (ADR 0027 §7).
    echo "Puffer des Compositors im Lasttest:"
    journalctl --user -u beize-compositor.service --since "$(cat "$out/load-start.txt")" \
        "${until[@]}" -o cat | grep '^held: ' | tail -n 2
fi
if [ -s "$out/rotate-start.txt" ]; then
    echo "Lasttest, Startfenster je Runde gewechselt:"
    until=()
    [ -s "$out/resize-start.txt" ] && until=(--until "$(cat "$out/resize-start.txt")")
    journalctl --user -u beize-compositor.service --since "$(cat "$out/rotate-start.txt")" \
        "${until[@]}" -o cat | grep '^pacing: ' | sort -t ' ' -k 2 -n | tail -n 1
    grep -h -E ': (load|window [0-9]+): ' "$out/rotate.log" | sed 's/^.*\]: //'
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
# With explicit sync no frame should wait for clients' later work
# (implicit sync over-waited on nouveau, CLAUDE.md §11); what is left
# stalls between the commit and the GPU.
echo "Späte Bilder: $(grep -c 'late frame (' "$out/compositor.log"), davon auf Clients" \
    "gewartet: $(grep -c -E 'waited for [1-9][0-9]* client buffers' "$out/compositor.log")"
grep -o -E 'stalled [0-9.]+' "$out/compositor.log" | cut -d ' ' -f 2 | sort -n |
    awk '{ v[NR] = $1 } END { if (NR) printf "stalled ms: median %.2f, max %.2f\n", v[int((NR + 1) / 2)], v[NR] }'
# nouveau's flip event may repeat the vblank before (no hardware counter,
# CLAUDE.md §11): how often, and the first ones with their times.
echo "Flip-Ereignisse mit dem vblank davor:" \
    "$(grep -o -E '[0-9]+ flip events repeated' "$out/compositor.log" | awk '{ n += $1 } END { print n + 0 }')"
grep -h 'flip event repeated' "$out/compositor.log" | sed 's/^.*\]: //' | head -n 5
echo "Tasten im Fenster: $(grep -c ' key ' "$out/client.log"), Klicks: $(grep -c 'button ' "$out/client.log")"
echo "Warnungen des Compositors:"
journalctl --user $b -u beize-compositor.service -p warning -o cat 2>/dev/null | sort | uniq -c
sync
