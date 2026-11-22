#!/bin/sh
# A songrec that reports one audio error and then carries on working. One
# error is not a dead device. Arguments are ignored.
echo '[2026-11-22T07:11:37Z ERROR songrec::cli_main src/cli_main.rs:180] Error: Audio error: an overrun' >&2
sleep 300
