// path_clean_count: go counterpart of bench/path_clean_count.pith.
//
// counts the segments path.Clean keeps for the same twelve messy paths,
// `rounds` times, weighting each count by the path's position. go has no
// counting variant, so this cleans the path and counts the non-empty
// segments of the result, which is what a go program wanting that number
// would write. path.Clean returns "." for an empty result, which counts as
// zero here, matching std.os.path.clean_part_count.
//
// build: go build -o bench/path_clean_count_go bench/path_clean_count.go
// run:   ./bench/path_clean_count_go <rounds>
package main

import (
	"fmt"
	"os"
	"path"
	"strconv"
	"time"
)

func cleanPartCount(p string) int {
	if p == "" {
		return 0
	}
	cleaned := path.Clean(p)
	if cleaned == "." {
		return 0
	}
	count := 0
	start := 0
	for i := 0; i <= len(cleaned); i++ {
		if i == len(cleaned) || cleaned[i] == '/' {
			if i > start {
				count++
			}
			start = i + 1
		}
	}
	return count
}

func main() {
	rounds := 20000
	if len(os.Args) > 1 {
		if v, err := strconv.Atoi(os.Args[1]); err == nil && v > 0 {
			rounds = v
		}
	}
	paths := []string{
		"data//north/./users/../users/123.json",
		"/var/log/../lib/./pith/build/",
		"../../escape/../still/../../up",
		"/../root/./sees/no/parent",
		"plain/relative/path",
		"a/b/c/d/e/f/g/h/../../../../x",
		"",
		"/",
		".",
		"./only/dots/./././end/.",
		"trailing/slashes///",
		"/abs//double/../../../gone",
	}
	start := time.Now()
	checksum := 0
	for round := 0; round < rounds; round++ {
		for i, p := range paths {
			checksum += cleanPartCount(p) * (i + 1)
		}
	}
	elapsed := time.Since(start).Milliseconds()
	fmt.Println("path clean_part_count benchmark")
	fmt.Printf("rounds=%d\n", rounds)
	fmt.Printf("elapsed_ms=%d\n", elapsed)
	fmt.Printf("checksum=%d\n", checksum)
}
