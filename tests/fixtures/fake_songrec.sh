#!/bin/sh
# tests/fixtures/fake_songrec.sh
# Stands in for the real `songrec` binary in tests: prints canned JSON
# match lines (with a repeat, to exercise dedup) and a log line, then exits.
echo '{"track": {"title": "Song A", "subtitle": "Artist A"}}'
sleep 0.05
echo '[2026-07-05T00:00:00Z INFO songrec::cli_main] some log line'
sleep 0.05
echo '{"track": {"title": "Song A", "subtitle": "Artist A"}}'
sleep 0.05
echo '{"track": {"title": "Song B", "subtitle": "Artist B"}}'
