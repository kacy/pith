// substring_search: go counterpart of bench/substring_search.pith.
//
// strings.Contains and strings.Index over haystacks of 12, 80 and 4000 bytes
// with the needle at the start, in the middle, at the end, or absent, for
// three needles: "comma" (one byte), "word" (seven bytes whose first byte is
// common in the filler) and "repeat" (a run of one byte against a needle that
// differs only in its last byte). every answer folds into one checksum, which
// matches the pith and rust versions for the same arguments.
//
// build: go build -o bench/substring_search_go bench/substring_search.go
// run:   ./bench/substring_search_go 20000 [cell|all] [comma|word|repeat|all]
package main

import (
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"
)

func argAt(at int, fallback string) string {
	if len(os.Args) > at {
		return os.Args[at]
	}
	return fallback
}

func intArg(at int, fallback int) int {
	if len(os.Args) > at {
		if v, err := strconv.Atoi(os.Args[at]); err == nil {
			return v
		}
	}
	return fallback
}

// filler with no comma and no occurrence of "sitting".
func filler(n int) string {
	pattern := "lorem ipsum dolor sit amet consectetur s "
	out := ""
	for len(out) < n {
		out += pattern
	}
	return out[:n]
}

func sizeOf(size string) int {
	switch size {
	case "short":
		return 12
	case "medium":
		return 80
	}
	return 4000
}

func haystackFor(n int, pos, needle string) string {
	if pos == "absent" {
		return filler(n)
	}
	gap := n - len(needle)
	switch pos {
	case "start":
		return needle + filler(gap)
	case "end":
		return filler(gap) + needle
	}
	left := gap / 2
	return filler(left) + needle + filler(gap-left)
}

func needleFor(kind string) string {
	switch kind {
	case "comma":
		return ","
	case "repeat":
		return "aaaaaab"
	}
	return "sitting"
}

func repeatHaystack(n int, pos string) string {
	if pos == "absent" {
		return strings.Repeat("a", n)
	}
	gap := n - 7
	switch pos {
	case "start":
		return "aaaaaab" + strings.Repeat("a", gap)
	case "end":
		return strings.Repeat("a", gap) + "aaaaaab"
	}
	left := gap / 2
	return strings.Repeat("a", left) + "aaaaaab" + strings.Repeat("a", gap-left)
}

type cell struct {
	name     string
	haystack string
	needle   string
}

func buildCells(only, needleKind string) []cell {
	var cells []cell
	for _, size := range []string{"short", "medium", "long"} {
		for _, pos := range []string{"start", "middle", "end", "absent"} {
			name := size + "-" + pos
			if only != "all" && only != name {
				continue
			}
			for _, kind := range []string{"comma", "word", "repeat"} {
				if needleKind != "all" && needleKind != kind {
					continue
				}
				needle := needleFor(kind)
				if kind == "repeat" {
					cells = append(cells, cell{name + "-" + kind, repeatHaystack(sizeOf(size), pos), needle})
				} else {
					cells = append(cells, cell{name + "-" + kind, haystackFor(sizeOf(size), pos, needle), needle})
				}
			}
		}
	}
	return cells
}

func run(cells []cell, rounds int) int {
	sum := 0
	for round := 0; round < rounds; round++ {
		for _, c := range cells {
			if strings.Contains(c.haystack, c.needle) {
				sum++
			}
			sum = (sum*31 + strings.Index(c.haystack, c.needle) + 2) % 1000000007
		}
	}
	return sum
}

func main() {
	rounds := intArg(1, 2000)
	only := argAt(2, "all")
	needleKind := argAt(3, "all")
	cells := buildCells(only, needleKind)
	calls := len(cells) * rounds * 2

	t0 := time.Now()
	sum := run(cells, rounds)
	elapsedNs := time.Since(t0).Nanoseconds()

	perCallNs := elapsedNs
	if calls > 0 {
		perCallNs = elapsedNs / int64(calls)
	}
	fmt.Printf("cells=%d rounds=%d calls=%d total_us=%d per_call_ns=%d checksum=%d\n",
		len(cells), rounds, calls, elapsedNs/1000, perCallNs, sum)
}
