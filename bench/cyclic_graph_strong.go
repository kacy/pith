// cyclic_graph_strong: go counterpart of bench/cyclic_graph_strong.pith.
//
// builds many two-node parent<->child rings and drops them. go is garbage
// collected, so the pith `weak` back edge has no counterpart here: both
// edges are plain pointers and the collector reclaims the cycle. this file
// and bench/cyclic_graph.go are therefore the same program; the weak
// and strong pith variants both map to it.
//
// build: go build -o bench/cyclic_graph_strong_go bench/cyclic_graph_strong.go
// run:   ./bench/cyclic_graph_strong_go 10000000
package main

import (
	"fmt"
	"os"
	"strconv"
)

type Node struct {
	value  int64
	next   *Node
	parent *Node
}

// build one ring and drop it. the checksum reads the node values directly,
// as the pith version does.
func buildRing(seed int64) int64 {
	a := &Node{value: seed}
	b := &Node{value: seed + 1}
	a.next = b
	b.parent = a
	return a.value + b.value
}

func main() {
	rings := int64(1000000)
	if len(os.Args) > 1 {
		if v, err := strconv.ParseInt(os.Args[1], 10, 64); err == nil && v > 0 {
			rings = v
		}
	}
	var checksum int64
	for i := int64(0); i < rings; i++ {
		checksum += buildRing(i)
	}
	fmt.Printf("rings: %d checksum: %d\n", rings, checksum)
}
