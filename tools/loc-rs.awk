# count rust source lines, skipping comments and blanks.
#
# by default counts library code and skips test modules; with -v want=test
# it counts the test modules instead. a test module is everything from the
# first `#[cfg(test)]` to the end of the file, which is where rust puts
# them by convention.
#
# the pith counterpart is tools/loc.awk. both are used by `make
# status-audit`, which is what the README's line counts cite.

FNR == 1 { in_test = 0 }

/#\[cfg\(test\)\]/ { in_test = 1 }

/^[[:space:]]*\/\// { next }
/^[[:space:]]*$/ { next }

in_test { if (want == "test") n++; next }
        { if (want != "test") n++ }

END { print n + 0 }
