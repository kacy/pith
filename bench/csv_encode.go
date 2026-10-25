// csv_encode: go counterpart of bench/csv_encode.pith.
//
// builds the same ten-column table outside the timed section and encodes
// it `rounds` times with encoding/csv into a bytes.Buffer. the checksum is
// the encoded byte count folded with every 97th byte, as in the pith
// version. encoding/csv ends every record with a newline while std.csv
// puts none after the last row, so the trailing newline is dropped before
// the checksum; the encoded bytes are otherwise identical (nothing in this
// table starts with a space or holds a carriage return, the two cases where
// the two encoders' quoting rules differ).
//
// build: go build -o bench/csv_encode_go bench/csv_encode.go
// run:   ./bench/csv_encode_go <rows> <rounds>
package main

import (
	"bytes"
	"encoding/csv"
	"fmt"
	"os"
	"strconv"
	"time"
)

func argOr(index, fallback int) int {
	if len(os.Args) > index {
		if v, err := strconv.Atoi(os.Args[index]); err == nil && v > 0 {
			return v
		}
	}
	return fallback
}

func regionFor(i int) string {
	switch i % 4 {
	case 0:
		return "north"
	case 1:
		return "south"
	case 2:
		return "east"
	}
	return "west"
}

func makeRows(records int) [][]string {
	rows := [][]string{{"id", "name", "region", "active", "score", "quota", "url", "joined", "note", "path"}}
	for i := 0; i < records; i++ {
		region := regionFor(i)
		id := strconv.Itoa(i)
		score := strconv.Itoa((i*17 + 11) % 100)
		quota := strconv.Itoa((i*13 + 7) % 50)
		note := "user " + id + ", region " + region
		url := "https://example.com/api/" + region + "/users/" + id + "?score=" + score
		joined := "2026-" + strconv.Itoa(i%12+1) + "-" + strconv.Itoa(i%28+1)
		userPath := "data//" + region + "/./users/../users/" + id + ".json"
		rows = append(rows, []string{id, "user-" + id, region, strconv.FormatBool(i%3 != 0), score, quota, url, joined, note, userPath})
	}
	return rows
}

func main() {
	records := argOr(1, 5000)
	rounds := argOr(2, 10)
	rows := makeRows(records)
	start := time.Now()
	checksum := 0
	for round := 0; round < rounds; round++ {
		var buf bytes.Buffer
		w := csv.NewWriter(&buf)
		if err := w.WriteAll(rows); err != nil {
			panic(err)
		}
		encoded := buf.Bytes()
		encoded = encoded[:len(encoded)-1]
		checksum += len(encoded)
		for i := round % 7; i < len(encoded); i += 97 {
			checksum += int(encoded[i])
		}
	}
	elapsed := time.Since(start).Milliseconds()
	fmt.Println("csv encode benchmark")
	fmt.Printf("rows=%d\n", records)
	fmt.Printf("rounds=%d\n", rounds)
	fmt.Printf("elapsed_ms=%d\n", elapsed)
	fmt.Printf("checksum=%d\n", checksum)
}
