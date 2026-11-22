#!/bin/sh
# A songrec whose audio device went away under it: it does not exit, it
# reports the same audio error on stderr in a tight loop, as songrec does
# after the audio server it records from restarts. Arguments are ignored.
while :; do
    echo '[2026-11-22T07:11:37Z ERROR songrec::cli_main src/cli_main.rs:180] Error: Audio error: A backend-specific error has occurred: `alsa::poll()` returned POLLERR' >&2
done
