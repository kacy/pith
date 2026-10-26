// string_build: go counterpart of bench/string_build.pith.
//
// grows one string by many short appends through strings.Builder.
//
// build: go build -o bench/string_build_go bench/string_build.go
// run:   ./bench/string_build_go 2000000
package main

import (
	"fmt"
	"os"
	"strconv"
	"strings"
)

func main() {
	n := 1000
	if len(os.Args) > 1 {
		if v, err := strconv.Atoi(os.Args[1]); err == nil && v > 0 {
			n = v
		}
	}
	var b strings.Builder
	for i := 0; i < n; i++ {
		b.WriteString("item-")
		b.WriteString(strconv.Itoa(i))
		b.WriteString(",")
	}
	s := b.String()
	sum := 0
	for j := 0; j < len(s); j += 97 {
		sum += int(s[j])
	}
	fmt.Printf("len: %d checksum: %d\n", len(s), sum)
}
