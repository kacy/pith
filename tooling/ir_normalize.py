#!/usr/bin/env python3
"""normalize one IR dump so two compilers' output can be compared for meaning.

six things move between two builds without changing what the program does:
register numbers (any insertion renumbers everything after it), label numbers
(they come off one counter for the whole module, so emitting a function
earlier renumbers every label in it), the order functions are emitted in (a
specialization queued from a different site lands in a different place), the
spelling of a specialization's type-argument suffix (`first__int` versus
`first__opt_int`), string-table indices (`strref 5 m0s12`: an interned
literal added anywhere shifts every index after it, and the string prepass
interns per declaration, so a std struct gaining a field shifts a program
that never mentions it), and the node-numbered suffix of an emitter temp
(`__json_decode_nested_71_12`: the ast node index and a counter, which move
with any edit to the source above the site). this rewrites all six to a fixed
form: every integer token becomes `N`, every `name__<suffix>` symbol becomes
`name__S`, every string reference becomes the literal it names (`str("...")`)
and the string table itself becomes the sorted set of its literals, every
temp's numeric suffix becomes `_N_N`, each function's labels are renumbered
from `L1` in order of first appearance within that function, and functions
are sorted by their normalized text.

labels are renumbered per function rather than blanked, so a branch to the
wrong label is still a difference; they are renumbered per function rather
than per module so that reordering two whole function bodies normalizes away,
which is what it means for the reordering to be meaningless.

what it cannot see: a body that splits into two (`first__tuple` becoming
`first__opt_int` and `first__opt_string`) shows as one added function, and a
symbol whose whole suffix is empty (`engine__`) keeps its spelling, so a rename
from an empty suffix shows as a difference. both are intended: they are real
changes to what is emitted.

usage: ir_normalize.py <ir file>      (normalized text on stdout, the count of
                                        distinct specialization symbols on stderr)
"""
import re
import sys

text = open(sys.argv[1]).read()

# string references first: `string m0s12 "lit"` lines define the table, and
# every `m<mod>s<idx>` token in the body reads one entry. inline the literal
# so an index shift is invisible and an added literal shows once, as one
# added line in the sorted table.
string_line = re.compile(r'(?m)^string (m\d+s\d+) (".*")$')
literals = {m.group(1): m.group(2) for m in string_line.finditer(text)}
if literals:
    table = "".join("string " + literals[k] + "\n" for k in sorted(literals, key=lambda k: literals[k]))
    text = string_line.sub("", text)
    # the removed lines leave blank ones, and a blank line means nothing
    text = re.sub(r"(?m)^\n", "", text)
    # a reference the table does not define (the consumer resolves it to a
    # null string) folds to one marker, since its number is all that moves
    text = re.sub(r"\bm\d+s\d+\b", lambda m: "str(" + literals.get(m.group(0), "<undefined>") + ")", text)
    text = table + text

# emitter temps carry the ast node index and a counter: `__catch_71_12`
temp = re.compile(r"\b(__[A-Za-z_]+?)_\d+_\d+\b")
text = temp.sub(lambda m: m.group(1) + "_N_N", text)

sym = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*?)__[A-Za-z0-9_]+\b")
distinct = set(m.group(0) for m in sym.finditer(text) if not m.group(0).startswith("__"))
text = sym.sub(lambda m: m.group(1) + "__S" if not m.group(0).startswith("__") else m.group(0), text)

label = re.compile(r"\bL\d+\b")


def renumber_labels(chunk):
    """rewrite L<n> to L1, L2, ... in order of first appearance in this chunk."""
    seen = {}

    def rename(m):
        name = m.group(0)
        if name not in seen:
            seen[name] = "L" + str(len(seen) + 1)
        return seen[name]

    return label.sub(rename, chunk)


parts = re.split(r"(?m)^(?=func )", text)
head, funcs = parts[0], parts[1:]
norm_funcs = sorted(re.sub(r"\b\d+\b", "N", renumber_labels(f)) for f in funcs)
head = re.sub(r"\b\d+\b", "N", renumber_labels(head))
sys.stdout.write(head + "".join(norm_funcs))
sys.stderr.write(str(len(distinct)) + "\n")
