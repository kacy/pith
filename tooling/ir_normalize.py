#!/usr/bin/env python3
"""normalize one IR dump so two compilers' output can be compared for meaning.

three things move between two builds without changing what the program does:
register numbers (any insertion renumbers everything after it), the order
functions are emitted in (a specialization queued from a different site lands
in a different place), and the spelling of a specialization's type-argument
suffix (`first__int` versus `first__opt_int`). this rewrites all three to a
fixed form: every integer token becomes `N`, every `name__<suffix>` symbol
becomes `name__S`, and functions are sorted by their normalized text.

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
sym = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*?)__[A-Za-z0-9_]+\b")
distinct = set(m.group(0) for m in sym.finditer(text) if not m.group(0).startswith("__"))
text = sym.sub(lambda m: m.group(1) + "__S" if not m.group(0).startswith("__") else m.group(0), text)

parts = re.split(r"(?m)^(?=func )", text)
head, funcs = parts[0], parts[1:]
norm_funcs = sorted(re.sub(r"\b\d+\b", "N", f) for f in funcs)
head = re.sub(r"\b\d+\b", "N", head)
sys.stdout.write(head + "".join(norm_funcs))
sys.stderr.write(str(len(distinct)) + "\n")
