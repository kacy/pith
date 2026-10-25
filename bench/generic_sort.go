// generic_sort: go counterpart of bench/generic_sort.pith.
//
// same rows (a seeded 31-bit lcg, built before the clock starts), same
// arms and same output line, so bench/generic_sort_bench.sh can drive this
// binary with BIN=./bench/generic_sort_go. `legacy` is the selection sort
// (int key) and insertion sort (string key) the pith program carries,
// written as generic functions over a key func. `current` is what a go
// program would call: copy the slice and slices.SortStableFunc on the
// field. both sorts are stable, like the pith merge sort, and the checksum
// folds the whole result so a different order shows up as a different
// number.
//
// build: go build -o bench/generic_sort_go bench/generic_sort.go
// run:   ./bench/generic_sort_go <arm> <dist> <key> <n> <rounds>
//
//	arm   legacy | current
//	dist  sorted | reverse | random | dups
//	key   int | string
package main

import (
	"cmp"
	"fmt"
	"os"
	"slices"
	"strconv"
	"strings"
	"time"
)

type Row struct {
	label string
	rank  int
}

// selection sort, one key call per surviving element per round.
func legacySortByKey[T any](items []T, key func(T) int) []T {
	result := []T{}
	used := map[int]bool{}
	for len(result) < len(items) {
		found := false
		bestIdx, bestKey := 0, 0
		for i, item := range items {
			if used[i] {
				continue
			}
			k := key(item)
			if !found || k < bestKey {
				bestIdx, bestKey, found = i, k, true
			}
		}
		if !found {
			return result
		}
		result = append(result, items[bestIdx])
		used[bestIdx] = true
	}
	return result
}

// insertion sort, re-reading the key of every element it probes.
func legacySortByKeyString[T any](items []T, key func(T) string) []T {
	out := []T{}
	for _, item := range items {
		k := key(item)
		placed := false
		for i := 0; i < len(out) && !placed; i++ {
			if k < key(out[i]) {
				out = slices.Insert(out, i, item)
				placed = true
			}
		}
		if !placed {
			out = append(out, item)
		}
	}
	return out
}

func labelFor(rank int) string {
	padded := "0000000" + strconv.Itoa(rank)
	return "row-" + padded[len(padded)-7:]
}

func rowsFor(dist string, n int) []Row {
	ranks := make([]int, 0, n)
	state := 2463534242
	for i := 0; i < n; i++ {
		switch dist {
		case "sorted":
			ranks = append(ranks, i)
		case "reverse":
			ranks = append(ranks, n-i)
		case "dups":
			state = (state*1103515245 + 12345) % 2147483648
			ranks = append(ranks, state%8)
		default:
			state = (state*1103515245 + 12345) % 2147483648
			ranks = append(ranks, state%1000000)
		}
	}
	rows := make([]Row, 0, n)
	for _, r := range ranks {
		rows = append(rows, Row{labelFor(r), r})
	}
	return rows
}

func rowRank(r Row) int     { return r.rank }
func rowLabel(r Row) string { return r.label }

func checksum(rows []Row) int {
	total := 0
	for i, r := range rows {
		total = (total + (i+1)*(r.rank+1) + len(r.label)) % 1000000007
	}
	return total
}

func argAt(at int, fallback string) string {
	if len(os.Args) > at {
		return os.Args[at]
	}
	return fallback
}

func intArg(at, fallback int) int {
	if v, err := strconv.Atoi(argAt(at, "")); err == nil {
		return v
	}
	return fallback
}

func main() {
	arm := argAt(1, "current")
	dist := argAt(2, "random")
	keyKind := argAt(3, "int")
	n := intArg(4, 1000)
	rounds := intArg(5, 5)

	rows := rowsFor(dist, n)

	sum := 0
	t0 := time.Now()
	for round := 0; round < rounds; round++ {
		var sorted []Row
		if keyKind == "string" {
			if arm == "legacy" {
				sorted = legacySortByKeyString(rows, rowLabel)
			} else {
				sorted = slices.Clone(rows)
				slices.SortStableFunc(sorted, func(a, b Row) int { return strings.Compare(a.label, b.label) })
			}
		} else {
			if arm == "legacy" {
				sorted = legacySortByKey(rows, rowRank)
			} else {
				sorted = slices.Clone(rows)
				slices.SortStableFunc(sorted, func(a, b Row) int { return cmp.Compare(a.rank, b.rank) })
			}
		}
		sum = (sum + checksum(sorted)) % 1000000007
	}
	elapsedUs := time.Since(t0).Nanoseconds() / 1000

	perOpUs := elapsedUs
	if rounds > 0 {
		perOpUs = elapsedUs / int64(rounds)
	}
	fmt.Printf("%s %s %s n=%d rounds=%d total_us=%d per_op_us=%d checksum=%d\n", arm, dist, keyKind, n, rounds, elapsedUs, perOpUs, sum)
}
