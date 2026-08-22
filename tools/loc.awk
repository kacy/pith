# count pith source lines, skipping comments and blanks.
#
# by default counts library code and skips colocated `test` blocks; with
# -v want=test it counts the test blocks instead. a test block starts at
# column zero with `test` and runs until the next line at column zero, so
# the split matches how the compiler sees them.
#
# used by `make status-audit`, which is what the README's line counts cite.

/^test[ \t]/ { in_test = 1; if (want == "test") n++; next }

in_test && /^[^ \t]/ { in_test = 0 }

/^[[:space:]]*#/ { next }
/^[[:space:]]*$/ { next }

in_test  { if (want == "test") n++; next }
         { if (want != "test") n++ }

END { print n + 0 }
