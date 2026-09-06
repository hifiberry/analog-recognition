#!/bin/sh
# A songrec that starts, prints nothing, and never exits: the state a PipeWire
# restart leaves behind. Its stdout never closes, so the stdout-watching
# supervision alone can never notice it has stopped working.
# Arguments are ignored on purpose -- it is stood up in place of songrec and
# is handed songrec's full argument list.
sleep 300
