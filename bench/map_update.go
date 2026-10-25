// map_update: go counterpart of bench/map_update.pith.
//
// the event ledger's map update in isolation: check whether the key is
// present, then store the old count plus one, over a small pool of keys
// repeated many times. the string flavor uses map[string]int, the int
// flavor map[int]int. go map keys cannot be []byte, so the bytes flavor
// keeps its keys as []byte and converts at each map access with
// string(k), the usual go idiom. the compiler skips the copy for the
// lookup but not for the store, so each update in that flavor allocates
// one key copy (visible in GODEBUG=gctrace=1), which is part of what it
// measures.
//
// build: go build -o bench/map_update_go bench/map_update.go
// run:   ./bench/map_update_go <flavor: string|int|bytes> <updates> <distinct keys>
package main

import (
	"fmt"
	"os"
	"strconv"
)

func argText(i int, fallback string) string {
	if len(os.Args) > i {
		return os.Args[i]
	}
	return fallback
}

func argInt(i, fallback int) int {
	if v, err := strconv.Atoi(argText(i, "")); err == nil && v > 0 {
		return v
	}
	return fallback
}

func keyText(i int) string {
	return "user-" + strconv.Itoa(i) + "-region"
}

func runString(updates, distinct int) int {
	keys := make([]string, 0, distinct)
	for i := 0; i < distinct; i++ {
		keys = append(keys, keyText(i))
	}
	m := map[string]int{}
	for i := 0; i < updates; i++ {
		k := keys[i%distinct]
		if v, ok := m[k]; ok {
			m[k] = v + 1
		} else {
			m[k] = 1
		}
	}
	total := 0
	for _, v := range m {
		total += v
	}
	return total
}

func runBytes(updates, distinct int) int {
	keys := make([][]byte, 0, distinct)
	for i := 0; i < distinct; i++ {
		keys = append(keys, []byte(keyText(i)))
	}
	m := map[string]int{}
	for i := 0; i < updates; i++ {
		k := keys[i%distinct]
		if v, ok := m[string(k)]; ok {
			m[string(k)] = v + 1
		} else {
			m[string(k)] = 1
		}
	}
	total := 0
	for _, v := range m {
		total += v
	}
	return total
}

func runInt(updates, distinct int) int {
	m := map[int]int{}
	for i := 0; i < updates; i++ {
		k := i % distinct
		if v, ok := m[k]; ok {
			m[k] = v + 1
		} else {
			m[k] = 1
		}
	}
	total := 0
	for _, v := range m {
		total += v
	}
	return total
}

func main() {
	flavor := argText(1, "string")
	updates := argInt(2, 200000)
	distinct := argInt(3, 64)
	var total int
	switch flavor {
	case "int":
		total = runInt(updates, distinct)
	case "bytes":
		total = runBytes(updates, distinct)
	default:
		total = runString(updates, distinct)
	}
	fmt.Printf("map_update flavor=%s updates=%d distinct=%d\n", flavor, updates, distinct)
	fmt.Printf("total=%d\n", total)
}
