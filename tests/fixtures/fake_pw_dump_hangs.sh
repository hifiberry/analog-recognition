#!/bin/sh
# A pw-dump that never returns, which is what a wedged (rather than absent)
# PipeWire daemon produces: it blocks on its core sync and prints nothing.
sleep 3600
