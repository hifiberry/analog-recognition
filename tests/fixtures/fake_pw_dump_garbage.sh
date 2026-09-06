#!/bin/sh
# A pw-dump that succeeds but says something this parser does not understand:
# a warning line ahead of the array, a truncated dump, or a future pw-dump with
# a different top-level shape all land here.
echo "WARNING: some node is misbehaving"
echo "[ {"
exit 0
