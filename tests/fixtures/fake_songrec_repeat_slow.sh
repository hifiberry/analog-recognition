#!/bin/sh
# tests/fixtures/fake_songrec_repeat_slow.sh
# Prints the same match three times with gaps wide enough for a test to
# reliably inject a reset notification between the 2nd and 3rd print.
echo '{"track": {"title": "Song A", "subtitle": "Artist A"}}'
sleep 0.2
echo '{"track": {"title": "Song A", "subtitle": "Artist A"}}'
sleep 0.2
echo '{"track": {"title": "Song A", "subtitle": "Artist A"}}'
