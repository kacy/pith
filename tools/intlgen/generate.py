#!/usr/bin/env python3
"""Generate the CLDR plural rule tables that std.intl compiles against.

Run from the repository root:

    python3 tools/intlgen/generate.py

This is separate from tools/unicodegen because it draws on a different body of
data with its own release cycle. Unicode says what a character is; CLDR says
what a language does with it, and the two are versioned independently.

## what is generated

CLDR gives cardinal plural rules for 224 locales, but only 40 distinct rule
sets between them -- English and German and Swedish all count the same way. So
the tables are a list of locales pointing at a much smaller list of rules.

Each rule is normalised into a compact form that std.intl evaluates directly:

    CLDR:  v = 0 and i % 10 = 2..4 and i % 100 != 12..14
    here:  v=0&i%10=2..4&i%100!12..14

`&` is "and", `|` is "or", `!` is "not equal". Keeping the shape of the
original means the generated data can be read against the CLDR source, rather
than being an opaque encoding that has to be trusted.

## verification

The generated rules are checked against the sample values CLDR publishes
alongside every rule -- the "@integer 0, 2~16, 100" lists. Those are the
standard's own statement of which numbers fall in which category, so a rule
that was mis-parsed or mis-normalised fails here rather than in someone's
message catalog.
"""

import json
import os
import re
import sys
import urllib.request

CLDR_VERSION = "48.2.1"

CLDR_BASE = f"https://raw.githubusercontent.com/unicode-org/cldr-json/{CLDR_VERSION}/cldr-json"
CACHE = os.path.join(os.path.dirname(os.path.abspath(__file__)), ".cldr-cache")
STD_INTL = os.path.join("std", "intl")

# the categories CLDR defines, in the order std.intl reports them. "other" is
# the fallback and never carries a condition.
CATEGORIES = ["zero", "one", "two", "few", "many", "other"]

RELATION = re.compile(r"^([nivwftce])\s*(?:%\s*(\d+))?\s*(!=|=)\s*(.+)$")


def fetch(path):
    os.makedirs(CACHE, exist_ok=True)
    local = os.path.join(CACHE, os.path.basename(path))
    if not os.path.exists(local):
        url = f"{CLDR_BASE}/{path}"
        sys.stderr.write(f"fetching {url}\n")
        with urllib.request.urlopen(url) as response:
            data = response.read()
        with open(local, "wb") as handle:
            handle.write(data)
    with open(local, encoding="utf-8") as handle:
        return json.load(handle)


def normalise(condition):
    """Turn one CLDR condition into the compact form std.intl evaluates."""
    condition = condition.split("@")[0].strip()
    if not condition:
        return ""

    groups = []
    for group in condition.split(" or "):
        relations = []
        for relation in group.split(" and "):
            match = RELATION.match(relation.strip())
            if not match:
                raise SystemExit(f"cannot parse plural relation: {relation!r}")
            operand, modulus, operator, ranges = match.groups()
            ranges = ranges.replace(" ", "")
            out = operand
            if modulus:
                out += "%" + modulus
            out += "=" if operator == "=" else "!"
            relations.append(out + ranges)
        groups.append("&".join(relations))
    return "|".join(groups)


def sample_values(rule):
    """The integer samples CLDR publishes for a rule, as a list of ints."""
    if "@integer" not in rule:
        return []
    text = rule.split("@integer")[1].split("@decimal")[0]
    values = []
    for item in text.split(","):
        item = item.strip()
        if not item or item == "…":
            continue
        if "~" in item:
            low, high = item.split("~")
            if "." in low or "." in high:
                continue
            values.extend(range(int(low), int(high) + 1))
        elif item.isdigit():
            values.append(int(item))
    return values


def evaluate(compact, value):
    """Evaluate a compact condition for an integer, mirroring std.intl."""
    if compact == "":
        return True
    for group in compact.split("|"):
        if all(evaluate_relation(relation, value) for relation in group.split("&")):
            return True
    return False


def evaluate_relation(relation, value):
    operand = relation[0]
    rest = relation[1:]
    modulus = 0
    if rest.startswith("%"):
        digits = re.match(r"%(\d+)", rest).group(1)
        modulus = int(digits)
        rest = rest[1 + len(digits) :]
    negate = rest[0] == "!"
    rest = rest[1:]

    # for an integer, n and i are the value and every other operand is zero
    left = value if operand in ("n", "i") else 0
    if modulus:
        left = left % modulus

    inside = False
    for part in rest.split(","):
        if ".." in part:
            low, high = part.split("..")
            if int(low) <= left <= int(high):
                inside = True
        elif left == int(part):
            inside = True
    return not inside if negate else inside


def build():
    data = fetch("cldr-core/supplemental/plurals.json")
    cardinal = data["supplemental"]["plurals-type-cardinal"]

    rule_sets = {}
    locales = []
    for locale in sorted(cardinal):
        rules = cardinal[locale]
        compact = []
        for category in CATEGORIES[:-1]:
            raw = rules.get(f"pluralRule-count-{category}")
            compact.append(normalise(raw) if raw else "")
        signature = tuple(compact)
        if signature not in rule_sets:
            rule_sets[signature] = len(rule_sets)
        locales.append((locale, rule_sets[signature]))

    ordered = [None] * len(rule_sets)
    for signature, index in rule_sets.items():
        ordered[index] = signature

    verify(cardinal, dict(locales), ordered)
    return locales, ordered, data["supplemental"]["version"]["_cldrVersion"]


def verify(cardinal, locale_index, rule_sets):
    """Check every rule against the sample values CLDR ships with it."""
    checked = 0
    for locale, rules in cardinal.items():
        chosen = rule_sets[locale_index[locale]]
        for category, raw in rules.items():
            category = category.replace("pluralRule-count-", "")
            for value in sample_values(raw):
                got = "other"
                for index, name in enumerate(CATEGORIES[:-1]):
                    if chosen[index] and evaluate(chosen[index], value):
                        got = name
                        break
                if got != category:
                    raise SystemExit(
                        f"{locale}: {value} should be {category!r} but the "
                        f"generated rules say {got!r}"
                    )
                checked += 1
    # CLDR ships a few thousand samples across all locales; a sharp drop
    # would mean the sample lists stopped being parsed rather than that the
    # rules got simpler.
    if checked < 5000:
        raise SystemExit(f"only {checked} sample values checked; expected several thousand")
    print(f"  verified {checked} CLDR sample values")


HEADER = """# generated by tools/intlgen/generate.py -- do not edit by hand
#
# cldr {version}
#
# regenerate with:
#     python3 tools/intlgen/generate.py
#
# cldr defines cardinal plural rules for {locales} locales, but only
# {sets} distinct rule sets between them. LOCALE_RULES maps a language
# to a rule set; PLURAL_RULES holds the rules themselves.
#
# a rule is a condition per category, in the order zero, one, two, few, many.
# "other" is the fallback and has no condition. within a condition, "|" is or,
# "&" is and, and "!" is not-equal, which keeps the generated text readable
# against the cldr source it came from:
#
#     cldr:  v = 0 and i % 10 = 2..4 and i % 100 != 12..14
#     here:  v=0&i%10=2..4&i%100!12..14
"""


def emit(locales, rule_sets, cldr_version):
    os.makedirs(STD_INTL, exist_ok=True)
    body = [
        HEADER.format(version=cldr_version, locales=len(locales), sets=len(rule_sets))
    ]

    # "af:0,agq:1,..." -- looked up by scanning, which is cheap next to the
    # work of rendering a message and keeps the table plain text.
    body.append("")
    body.append("# language subtag to rule set index, comma separated.")
    body.append('pub LOCALE_RULES := "' + ",".join(f"{name}:{index}" for name, index in locales) + '"')

    # one line per rule set, categories separated by ";"
    body.append("")
    body.append("# one rule set per \"/\", categories separated by \";\" in the order")
    body.append("# zero, one, two, few, many. a category the language does not use is")
    body.append("# written \"-\" rather than left blank: pith's String.split drops empty")
    body.append("# segments, so a blank would shift every later category up a slot.")
    body.append(
        'pub PLURAL_RULES := "'
        + "/".join(";".join(part if part else "-" for part in rule) for rule in rule_sets)
        + '"'
    )
    body.append("")
    body.append("# the number of rule sets in PLURAL_RULES.")
    body.append("pub fn plural_rule_count() -> Int:")
    body.append(f"    return {len(rule_sets)}")

    path = os.path.join(STD_INTL, "plural_tables.pith")
    with open(path, "w", encoding="utf-8") as handle:
        handle.write("\n".join(body) + "\n")
    return path, os.path.getsize(path)


def main():
    if not os.path.isdir("std"):
        raise SystemExit("run this from the repository root")
    locales, rule_sets, cldr_version = build()
    path, size = emit(locales, rule_sets, cldr_version)
    print(f"cldr {cldr_version}")
    print(f"  locales      {len(locales):5d}")
    print(f"  rule sets    {len(rule_sets):5d}")
    print(f"  {path} {size} bytes")


if __name__ == "__main__":
    main()
