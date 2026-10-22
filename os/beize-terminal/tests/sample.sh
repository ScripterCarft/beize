#!/bin/sh
# The terminal's sample (ADR 0027 §9 "Test"): what Beize's terminal draws,
# a line each. The smoke test types its name into a terminal and compares
# the screen with os/beize-terminal/tests/sample.png.

# Text: accents, umlauts, quotes, a combining mark (e + U+0301).
printf '%s' 'Grüße aus Beize: äöü ÄÖÜ ß é ñ € „Zitat“ – '
printf 'e\314\201\n'
# Attributes.
printf '\033[1mfett\033[0m \033[2mschwach\033[0m \033[4munterstrichen\033[0m '
printf '\033[9mdurchgestrichen\033[0m \033[7minvers\033[0m\n'
# The 16 colours: as text, then as backgrounds, normal and bright.
printf '\033[31mrot \033[32mgrün \033[33mgelb \033[34mblau \033[35mmagenta \033[36mcyan\033[0m\n'
for c in 0 1 2 3 4 5 6 7; do printf '\033[4%sm   ' "$c"; done
printf '\033[0m\n'
for c in 0 1 2 3 4 5 6 7; do printf '\033[10%sm   ' "$c"; done
printf '\033[0m\n'
# xterm's grey ramp (232–255) and 24-bit colour.
c=232
while [ "$c" -le 255 ]; do printf '\033[48;5;%sm ' "$c"; c=$((c + 1)); done
printf '\033[0m\n'
i=0
while [ "$i" -lt 24 ]; do printf '\033[48;2;%s;%s;200m ' $((i * 11)) $((255 - i * 11)); i=$((i + 1)); done
printf '\033[0m\n'
# Boxes and blocks, which the terminal draws itself.
printf '%s\n' '╭────┬────╮ ┏━━━━┓ ╔════╗' '│ Ab │ Cd │ ┃ Ef ┃ ║ Gh ║  ░▒▓█ ▀▄▌▐' '╰────┴────╯ ┗━━━━┛ ╚════╝'
