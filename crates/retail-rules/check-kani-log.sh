#!/bin/sh
# Fails when a Kani log shows a vacuous proof or an impossible cover in this crate.
#
# Kani 0.68.0 reports `VERIFICATION:- SUCCESSFUL` and exits 0 in two cases that prove nothing:
# a `kani::cover!` that is `UNSATISFIABLE`, and an assertion that is `UNREACHABLE`, for example
# after a contradictory `kani::assume`. Library checks in `core` and `kani` are UNREACHABLE in
# normal runs, so only checks located in crates/retail-rules count.
#
# Usage: check-kani-log.sh <kani log>
set -eu
log=$1
grep -q '^Complete - [0-9]* successfully verified harnesses, 0 failures' "$log" || {
    echo "check-kani-log: no successful Kani summary in $log" >&2
    exit 1
}
awk '
    /^Check [0-9]+:/ { check = $0 }
    /- Status: UNSATISFIABLE/ { print "check-kani-log: unsatisfiable cover: " check; bad = 1 }
    /- Status: UNREACHABLE/ { unreachable = 1; next }
    unreachable && /- Location:/ {
        if ($0 ~ /crates\/retail-rules\//) { print "check-kani-log: unreachable check: " check " " $0; bad = 1 }
        unreachable = 0
    }
    END { exit bad }
' "$log" >&2
echo "check-kani-log: no unsatisfiable cover and no unreachable check in crates/retail-rules"
