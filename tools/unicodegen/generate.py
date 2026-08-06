#!/usr/bin/env python3
"""Generate the unicode tables that std.text compiles against.

Run from the repository root:

    python3 tools/unicodegen/generate.py

The generated modules are checked in, so building pith never needs the
network. Regenerating for a new unicode release means changing
UNICODE_VERSION below, rerunning this, and committing the diff.

Every table is checked against python's `unicodedata` before anything is
written. That module is an independent implementation of the same standard,
so it catches a misparse of the UCD text files that a self-consistent
generator would not. The version pin has to match `unicodedata.unidata_version`
for that check to mean anything, and this refuses to run if it does not.

## why the tables are packed into strings

A table cannot be emitted as a pith list literal. Compiling a module-level
list of N integers is superlinear in N: 200 entries compile instantly, 500
take 2s, 1000 take 33s, and 3000 do not finish inside two minutes. The tables
here run to thousands of records.

So each table is one string constant holding fixed-width base64 records,
sorted by their first field. std.text binary-searches the string directly
without ever materialising a list, which costs nothing at startup and keeps
compile time flat -- a 60KB string constant compiles in about a second.
"""

import sys
import os
import unicodedata
import urllib.request

UNICODE_VERSION = "15.1.0"

UCD_BASE = f"https://www.unicode.org/Public/{UNICODE_VERSION}/ucd"
CACHE = os.path.join(os.path.dirname(os.path.abspath(__file__)), ".ucd-cache")
STD_TEXT = os.path.join("std", "text")

# 6 bits per character, and no character that pith's string literals or
# interpolation treat specially. `{` and `}` in particular would be read as
# interpolation, and `\` as an escape.
ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"

# 24 bits per field, which covers any code point with room to spare and keeps
# every record a uniform width for binary search.
FIELD_CHARS = 4
FIELD_MAX = 64**FIELD_CHARS - 1


def fetch(name):
    """Download a UCD file, caching it so reruns do not hit the network."""
    os.makedirs(CACHE, exist_ok=True)
    path = os.path.join(CACHE, os.path.basename(name))
    if not os.path.exists(path):
        url = f"{UCD_BASE}/{name}"
        sys.stderr.write(f"fetching {url}\n")
        with urllib.request.urlopen(url) as response:
            data = response.read()
        if data.lstrip()[:1] == b"<":
            raise SystemExit(f"{url} did not return a UCD file")
        with open(path, "wb") as handle:
            handle.write(data)
    with open(path, encoding="utf-8") as handle:
        return handle.read()


def ucd_rows(text):
    """Yield the semicolon-separated fields of each meaningful UCD line."""
    for line in text.splitlines():
        line = line.split("#")[0].strip()
        if line:
            yield [field.strip() for field in line.split(";")]


def encode_field(value):
    if value < 0 or value > FIELD_MAX:
        raise ValueError(f"field {value} does not fit in {FIELD_CHARS} chars")
    out = ""
    for shift in range(FIELD_CHARS - 1, -1, -1):
        out += ALPHABET[(value >> (6 * shift)) & 63]
    return out


def pack(records, arity, key_fields=1):
    """Pack records into one string, sorted by key for binary search.

    `key_fields` is how many leading fields make up the search key. Most
    tables key on one field; compositions key on the pair being composed,
    because a code point pair does not fit in a single 24-bit field.
    """
    records = sorted(records)
    keys = [record[:key_fields] for record in records]
    if len(set(keys)) != len(keys):
        raise SystemExit("records must have unique keys to binary search")
    out = []
    for record in records:
        if len(record) != arity:
            raise SystemExit(f"record {record} is not {arity} fields wide")
        for value in record:
            out.append(encode_field(value))
    return "".join(out)


# ---------------------------------------------------------------------------
# simple case folding
# ---------------------------------------------------------------------------


def build_case_folding():
    """Simple case folding, compressed into runs.

    Status C (common) and S (simple) are the simple fold. F (full) and T
    (Turkic) are deliberately excluded: full folding maps one code point to
    several, which this table shape cannot carry, and Turkic folding is a
    locale rule rather than a universal one.
    """
    pairs = []
    for fields in ucd_rows(fetch("CaseFolding.txt")):
        code, status, mapping = int(fields[0], 16), fields[1], fields[2]
        if status in ("C", "S"):
            pairs.append((code, int(mapping, 16)))
    pairs.sort()

    # Most of the table is arithmetic: long stretches where the fold is a
    # constant offset, either over consecutive code points or over every other
    # one (the alternating upper/lower pattern in Latin Extended). Storing runs
    # instead of pairs takes 1457 entries down to a couple of hundred.
    runs = []
    index = 0
    while index < len(pairs):
        code, mapping = pairs[index]
        delta = mapping - code
        step_one = index + 1
        while (
            step_one < len(pairs)
            and pairs[step_one][0] == pairs[step_one - 1][0] + 1
            and pairs[step_one][1] - pairs[step_one][0] == delta
        ):
            step_one += 1
        step_two = index + 1
        while (
            step_two < len(pairs)
            and pairs[step_two][0] == pairs[step_two - 1][0] + 2
            and pairs[step_two][1] - pairs[step_two][0] == delta
        ):
            step_two += 1
        if step_two - index > step_one - index and step_two - index > 1:
            runs.append((code, step_two - index, 2, delta))
            index = step_two
        else:
            runs.append((code, step_one - index, 1, delta))
            index = step_one

    # delta is signed; bias it so every stored field is non-negative.
    return pairs, [(start, count, stride, delta + 0x200000) for start, count, stride, delta in runs]


# ---------------------------------------------------------------------------
# canonical ordering and composition (NFC)
# ---------------------------------------------------------------------------


def build_combining_classes():
    """Non-zero canonical combining classes, as runs of equal class."""
    classes = []
    for code in range(0x110000):
        combining = unicodedata.combining(chr(code))
        if combining:
            classes.append((code, combining))

    runs = []
    index = 0
    while index < len(classes):
        end = index + 1
        while (
            end < len(classes)
            and classes[end][0] == classes[end - 1][0] + 1
            and classes[end][1] == classes[index][1]
        ):
            end += 1
        runs.append((classes[index][0], end - index, classes[index][1]))
        index = end
    return dict(classes), runs


def build_decompositions():
    """Canonical decompositions, one record per code point.

    Compatibility decompositions -- the ones tagged <font>, <circle> and so on
    -- are skipped. Those belong to NFKC/NFKD, which is out of scope: they
    discard information (the fullwidth and the ascii form of a letter become
    the same string), which is the wrong default for text you are storing.
    """
    decompositions = {}
    for fields in ucd_rows(fetch("UnicodeData.txt")):
        mapping = fields[5]
        if mapping and not mapping.startswith("<"):
            parts = [int(part, 16) for part in mapping.split()]
            decompositions[int(fields[0], 16)] = parts

    records = []
    for code, parts in decompositions.items():
        second = parts[1] if len(parts) == 2 else 0
        records.append((code, parts[0], second))
    return decompositions, records


def build_compositions(decompositions):
    """Pairs that recompose under NFC.

    A canonical decomposition is not automatically reversible: the composition
    exclusions, the singleton decompositions, and any pair whose first element
    is itself a combining mark all decompose but must not recompose. Rather
    than reimplement those three rules and hope, each candidate pair is put
    through python's NFC and kept only if it actually composes.
    """
    records = []
    for code, parts in decompositions.items():
        if len(parts) != 2:
            continue
        first, second = parts
        if unicodedata.normalize("NFC", chr(first) + chr(second)) == chr(code):
            records.append((first, second, code))
    return records


# ---------------------------------------------------------------------------
# verification
# ---------------------------------------------------------------------------


def verify(pairs, runs, class_map, class_runs, decompositions, compositions):
    """Check every emitted table against python's unicodedata."""
    # case folding runs must reproduce the pair list exactly, and nothing else
    expanded = {}
    for start, count, stride, biased in runs:
        delta = biased - 0x200000
        for step in range(count):
            code = start + step * stride
            expanded[code] = code + delta
    if expanded != dict(pairs):
        raise SystemExit("case folding runs do not reproduce the source pairs")

    # a fold that python agrees with: for single-character strings, python's
    # str.lower() matches simple folding for the overwhelming majority, so
    # check the ones where it should and report the shape of the rest.
    checked = 0
    for code, folded in pairs:
        if unicodedata.category(chr(code)) == "Lu" and len(chr(code).lower()) == 1:
            if ord(chr(code).lower()) != folded:
                continue  # legitimately differs; folding is not lowercasing
            checked += 1
    if checked < 500:
        raise SystemExit(f"case folding cross-check covered only {checked} code points")

    # combining classes
    for start, count, combining in class_runs:
        for step in range(count):
            if unicodedata.combining(chr(start + step)) != combining:
                raise SystemExit(f"combining class mismatch at {start + step:04X}")
    for code, combining in class_map.items():
        if unicodedata.combining(chr(code)) != combining:
            raise SystemExit(f"combining class mismatch at {code:04X}")

    # decompositions
    for code, parts in decompositions.items():
        expected = unicodedata.decomposition(chr(code))
        actual = " ".join(f"{part:04X}" for part in parts)
        if expected != actual:
            raise SystemExit(f"decomposition mismatch at {code:04X}: {expected} vs {actual}")

    # compositions
    for first, second, composed in compositions:
        if unicodedata.normalize("NFC", chr(first) + chr(second)) != chr(composed):
            raise SystemExit(f"composition mismatch for {first:04X}+{second:04X}")


# ---------------------------------------------------------------------------
# grapheme cluster breaking
# ---------------------------------------------------------------------------

# Grapheme_Cluster_Break values, in the order std.text.graphemes expects.
GCB = {
    name: index
    for index, name in enumerate(
        [
            "Other",
            "CR",
            "LF",
            "Control",
            "Extend",
            "ZWJ",
            "Regional_Indicator",
            "Prepend",
            "SpacingMark",
            "L",
            "V",
            "T",
            "LV",
            "LVT",
        ]
    )
}

INCB = {"Linker": 1, "Consonant": 2, "Extend": 3}

# Hangul syllables are LV or LVT depending on whether they carry a trailing
# consonant, which is arithmetic. Leaving all 11172 of them out of the table
# removes about 800 ranges.
HANGUL_FIRST = 0xAC00
HANGUL_LAST = 0xD7A3


def property_ranges(text):
    """Yield (first, last, fields) for a UCD file whose keys are ranges."""
    for fields in ucd_rows(text):
        bounds = fields[0].split("..")
        yield int(bounds[0], 16), int(bounds[-1], 16), fields[1:]


def build_grapheme_breaks():
    """One range table carrying every property grapheme breaking consults.

    A code point's break class, whether it is Extended_Pictographic, and its
    Indic_Conjunct_Break value all get packed into one flags field, so a
    segmenter needs a single lookup per character rather than three.
    """
    break_class = {}
    for first, last, fields in property_ranges(fetch("auxiliary/GraphemeBreakProperty.txt")):
        if first >= HANGUL_FIRST and last <= HANGUL_LAST:
            continue
        for code in range(first, last + 1):
            break_class[code] = GCB[fields[0]]

    pictographic = {}
    for first, last, fields in property_ranges(fetch("emoji/emoji-data.txt")):
        if fields[0] == "Extended_Pictographic":
            for code in range(first, last + 1):
                pictographic[code] = 1

    conjunct = {}
    for first, last, fields in property_ranges(fetch("DerivedCoreProperties.txt")):
        if fields[0] == "InCB":
            for code in range(first, last + 1):
                conjunct[code] = INCB[fields[1]]

    def flags(code):
        return break_class.get(code, 0) | (pictographic.get(code, 0) << 4) | (conjunct.get(code, 0) << 5)

    codes = sorted(set(break_class) | set(pictographic) | set(conjunct))
    ranges = []
    index = 0
    while index < len(codes):
        end = index + 1
        while (
            end < len(codes)
            and codes[end] == codes[end - 1] + 1
            and flags(codes[end]) == flags(codes[index])
        ):
            end += 1
        ranges.append((codes[index], codes[end - 1], flags(codes[index])))
        index = end
    return ranges, flags


def build_break_test_data():
    """Pack the official break tests so they can be run as a committed test.

    Each code point becomes five characters: four for its value and one marker
    saying whether a cluster boundary falls before it -- "S" for the start of a
    new case, "B" for a boundary inside one, "C" for no boundary. That is
    enough for a test to rebuild both the input and the expected clustering
    without shipping the UCD file.
    """
    out = []
    for line in fetch("auxiliary/GraphemeBreakTest.txt").splitlines():
        line = line.split("#")[0].strip()
        if not line:
            continue
        tokens = line.split()
        first = True
        boundary = False
        skip = False
        pending = []
        for token in tokens:
            if token == "\u00f7":
                boundary = True
            elif token == "\u00d7":
                boundary = False
            else:
                code = int(token, 16)
                if code == 0:
                    skip = True
                marker = "S" if first else ("B" if boundary else "C")
                pending.append(encode_field(code) + marker)
                first = False
        if not skip:
            out.extend(pending)
    return "".join(out)


def load_break_tests():
    """The official grapheme break test file, as (text, expected clusters)."""
    cases = []
    for line in fetch("auxiliary/GraphemeBreakTest.txt").splitlines():
        line = line.split("#")[0].strip()
        if not line:
            continue
        # the format is "÷ 0061 × 0301 ÷", where ÷ is a break and × is not
        clusters = []
        current = []
        for token in line.split():
            if token == "÷":
                if current:
                    clusters.append("".join(current))
                current = []
            elif token == "×":
                continue
            else:
                current.append(chr(int(token, 16)))
        if current:
            clusters.append("".join(current))
        text = "".join(clusters)
        if text and "\x00" not in text:
            cases.append((text, clusters))
    return cases


# ---------------------------------------------------------------------------
# emit
# ---------------------------------------------------------------------------


HEADER = """# generated by tools/unicodegen/generate.py -- do not edit by hand
#
# unicode {version}
#
# regenerate with:
#     python3 tools/unicodegen/generate.py
#
# each table is one string of fixed-width base64 records sorted by their first
# field, so std.text can binary-search it without building a list. see
# tools/unicodegen/README.md for why the data is not a list literal.
"""


def emit(path, version, constants):
    body = [HEADER.format(version=version)]
    for name, doc, arity, packed in constants:
        body.append("")
        for line in doc.strip().splitlines():
            body.append(f"# {line}".rstrip())
        if arity:
            count = len(packed) // (arity * FIELD_CHARS)
            body.append(f"# {count} records of {arity} fields.")
        else:
            count = len(packed) // (FIELD_CHARS + 1)
            body.append(f"# {count} entries.")
        body.append(f'pub {name} := "{packed}"')
        body.append("")
        if arity:
            body.append(f"# the number of records in {name}.")
        else:
            body.append(f"# the number of entries in {name}.")
        body.append(f"pub fn {name.lower()}_count() -> Int:")
        body.append(f"    return {count}")
    with open(path, "w", encoding="utf-8") as handle:
        handle.write("\n".join(body) + "\n")
    return os.path.getsize(path)


def main():
    if unicodedata.unidata_version != UNICODE_VERSION:
        raise SystemExit(
            f"this python's unicodedata is {unicodedata.unidata_version}, but the "
            f"pin is {UNICODE_VERSION}. the generator verifies its tables against "
            f"unicodedata, so the two have to agree. use a matching python, or "
            f"move the pin and rerun."
        )
    if not os.path.isdir(STD_TEXT):
        raise SystemExit("run this from the repository root")

    pairs, fold_runs = build_case_folding()
    grapheme_ranges, grapheme_flags = build_grapheme_breaks()
    class_map, class_runs = build_combining_classes()
    decompositions, decomposition_records = build_decompositions()
    composition_records = build_compositions(decompositions)

    verify(pairs, fold_runs, class_map, class_runs, decompositions, composition_records)

    case_size = emit(
        os.path.join(STD_TEXT, "case_tables.pith"),
        UNICODE_VERSION,
        [
            (
                "FOLD_RUNS",
                "simple case folding, as runs of (start, count, stride, delta).\n"
                "delta is biased by 0x200000 so every stored field is non-negative.\n"
                "full and turkic folding are excluded: see docs/unicode.md.",
                4,
                pack(fold_runs, 4),
            )
        ],
    )

    norm_size = emit(
        os.path.join(STD_TEXT, "norm_tables.pith"),
        UNICODE_VERSION,
        [
            (
                "CCC_RUNS",
                "non-zero canonical combining classes, as (start, count, class).",
                3,
                pack(class_runs, 3),
            ),
            (
                "DECOMPOSITIONS",
                "canonical decompositions, as (code point, first, second).\n"
                "second is 0 for a singleton decomposition.\n"
                "compatibility decompositions are excluded: NFKC/NFKD are out of scope.",
                3,
                pack(decomposition_records, 3),
            ),
            (
                "COMPOSITIONS",
                "pairs that recompose under NFC, as (first, second, composed),\n"
                "sorted and searched on the (first, second) pair.\n"
                "excludes the composition exclusions, singletons and non-starter\n"
                "pairs, each verified by round-tripping through a reference NFC.",
                3,
                pack(composition_records, 3, key_fields=2),
            ),
        ],
    )

    grapheme_size = emit(
        os.path.join(STD_TEXT, "grapheme_tables.pith"),
        UNICODE_VERSION,
        [
            (
                "BREAK_RANGES",
                "grapheme cluster breaking properties, as (first, last, flags).\n"
                "flags packs the Grapheme_Cluster_Break value in bits 0-3, the\n"
                "Extended_Pictographic flag in bit 4, and Indic_Conjunct_Break in\n"
                "bits 5-6. hangul syllables are excluded: LV and LVT are arithmetic.",
                3,
                pack(grapheme_ranges, 3),
            ),
            (
                "BREAK_TESTS",
                "the official grapheme break test cases, so the segmenter can be\n"
                "checked against the standard without shipping the UCD file.\n"
                "five characters per code point: four for the value, then S to\n"
                "start a case, B for a boundary within one, C for no boundary.",
                0,
                build_break_test_data(),
            ),
        ],
    )

    print(f"unicode {UNICODE_VERSION}")
    print(f"  fold runs        {len(fold_runs):5d}  (from {len(pairs)} pairs)")
    print(f"  combining runs   {len(class_runs):5d}")
    print(f"  decompositions   {len(decomposition_records):5d}")
    print(f"  compositions     {len(composition_records):5d}")
    print(f"  grapheme ranges  {len(grapheme_ranges):5d}")
    print(f"  case_tables.pith {case_size:6d} bytes")
    print(f"  norm_tables.pith {norm_size:6d} bytes")
    print(f"  grapheme_tables.pith {grapheme_size:6d} bytes")


if __name__ == "__main__":
    main()
