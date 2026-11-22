#!/bin/sh
# A songrec that fails at start: it says why on stderr and exits, as when
# its device is not there yet. Arguments are ignored.
echo '[2026-11-22T07:11:37Z ERROR songrec::cli_main src/cli_main.rs:136] Exiting: audio device not found' >&2
exit 1
