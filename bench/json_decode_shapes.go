// json_decode_shapes: go counterpart of bench/json_decode_shapes.pith.
//
// typed json decoding, one struct shape per run, with encoding/json: each
// round calls json.Unmarshal into a fresh value of the shape's struct, on
// the same document bytes. encoding/json has no generic tree to decode a
// struct out of, so `list_walk` reads `items` as []json.RawMessage and
// unmarshals each element into Small; that scans every element twice,
// where the pith version parses once into its node pool. the documents,
// shapes and checksum are the same as the pith program's, so equal output
// means equal decoding.
//
// build: go build -o bench/json_decode_shapes_go bench/json_decode_shapes.go
// run:   ./bench/json_decode_shapes_go <shape> <size> <rounds>
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"strings"
)

type Small struct {
	ID     int    `json:"id"`
	Name   string `json:"name"`
	Active bool   `json:"active"`
}

type Wide struct {
	F01 int    `json:"f01"`
	F02 string `json:"f02"`
	F03 bool   `json:"f03"`
	F04 int    `json:"f04"`
	F05 string `json:"f05"`
	F06 int    `json:"f06"`
	F07 int    `json:"f07"`
	F08 string `json:"f08"`
	F09 bool   `json:"f09"`
	F10 int    `json:"f10"`
	F11 string `json:"f11"`
	F12 int    `json:"f12"`
	F13 int    `json:"f13"`
	F14 string `json:"f14"`
	F15 bool   `json:"f15"`
	F16 int    `json:"f16"`
	F17 string `json:"f17"`
	F18 int    `json:"f18"`
	F19 int    `json:"f19"`
	F20 string `json:"f20"`
}

type Inner struct {
	City string `json:"city"`
	Zip  int    `json:"zip"`
}

type Nested struct {
	Name  string `json:"name"`
	Addr  Inner  `json:"addr"`
	Score int    `json:"score"`
}

type Sample struct {
	ID    int     `json:"id"`
	Name  string  `json:"name"`
	Value float64 `json:"value"`
	Delta float64 `json:"delta"`
}

type SampleInts struct {
	ID    int    `json:"id"`
	Name  string `json:"name"`
	Value int    `json:"value"`
	Delta int    `json:"delta"`
}

type Items struct {
	Items []Small `json:"items"`
}

type RawItems struct {
	Items []json.RawMessage `json:"items"`
}

type Ids struct {
	Name string `json:"name"`
	Ids  []int  `json:"ids"`
}

type Grid struct {
	Name  string  `json:"name"`
	Cells [][]int `json:"cells"`
}

type Scores struct {
	Name   string         `json:"name"`
	Scores map[string]int `json:"scores"`
}

type Catalog struct {
	Name  string           `json:"name"`
	Items map[string]Small `json:"items"`
}

func textOf(width int) string {
	b := make([]byte, width)
	for i := range b {
		b[i] = byte('a' + i%26)
	}
	return string(b)
}

func smallDoc(width int) string {
	return `{"id":7,"name":"` + textOf(width) + `","active":true}`
}

func wideDoc(width int) string {
	text := `"` + textOf(width) + `"`
	fields := make([]string, 0, 20)
	for i := 1; i <= 20; i++ {
		var value string
		switch {
		case i%3 == 2:
			value = text
		case i%6 == 3:
			value = "true"
		default:
			value = strconv.Itoa(i * 11)
		}
		fields = append(fields, fmt.Sprintf(`"f%d%d":%s`, i/10, i%10, value))
	}
	return "{" + strings.Join(fields, ",") + "}"
}

func nestedDoc(width int) string {
	t := textOf(width)
	return `{"name":"` + t + `","addr":{"city":"` + t + `","zip":75001},"score":42}`
}

func sampleDoc(width int, floats bool) string {
	value, delta := "1250", "-2500"
	if floats {
		value, delta = "12.5", "-0.25"
	}
	return `{"id":7,"name":"` + textOf(width) + `","value":` + value + `,"delta":` + delta + `}`
}

func joined(count int, item func(int) string) string {
	parts := make([]string, count)
	for i := range parts {
		parts[i] = item(i)
	}
	return strings.Join(parts, ",")
}

func listDoc(count int) string {
	return `{"items":[` + joined(count, func(i int) string {
		return fmt.Sprintf(`{"id":%d,"name":"item%d","active":false}`, i, i)
	}) + `]}`
}

func listScalarDoc(count int) string {
	return `{"name":"ids","ids":[` + joined(count, func(i int) string { return strconv.Itoa(i * 7) }) + `]}`
}

func nestedListDoc(count int) string {
	return `{"name":"grid","cells":[` + joined(count, func(i int) string {
		return fmt.Sprintf("[%d,%d,%d,%d]", i, i*2, i*3, i*4)
	}) + `]}`
}

func mapDoc(count int) string {
	return `{"name":"scores","scores":{` + joined(count, func(i int) string {
		return fmt.Sprintf(`"k%d":%d`, i, i*7)
	}) + `}}`
}

func mapStructDoc(count int) string {
	return `{"name":"catalog","items":{` + joined(count, func(i int) string {
		return fmt.Sprintf(`"k%d":{"id":%d,"name":"item%d","active":false}`, i, i, i)
	}) + `}}`
}

func b2i(b bool) int {
	if b {
		return 1
	}
	return 0
}

func smallSum(r Small) int { return r.ID + len(r.Name) + b2i(r.Active) }

func wideSum(r Wide) int {
	v := r.F01 + r.F04 + r.F06 + r.F07 + r.F10 + r.F12 + r.F13 + r.F16 + r.F18 + r.F19
	v += len(r.F02) + len(r.F05) + len(r.F08) + len(r.F11) + len(r.F14) + len(r.F17) + len(r.F20)
	return v + b2i(r.F03) + b2i(r.F09) + b2i(r.F15)
}

func nestedSum(r Nested) int { return len(r.Name) + len(r.Addr.City) + r.Addr.Zip + r.Score }

func floatSum(r Sample) int {
	return r.ID + len(r.Name) + int(r.Value*10.0) + int(r.Delta*100.0)
}

func intsSum(r SampleInts) int { return r.ID + len(r.Name) + r.Value + r.Delta }

func listSum(r Items) int {
	v := 0
	for _, it := range r.Items {
		v += it.ID + len(it.Name)
	}
	return v
}

func listScalarSum(r Ids) int {
	v := len(r.Name)
	for _, id := range r.Ids {
		v += id
	}
	return v
}

func nestedListSum(r Grid) int {
	v := len(r.Name)
	for _, row := range r.Cells {
		for _, x := range row {
			v += x
		}
	}
	return v
}

func mapSum(r Scores) int {
	v := len(r.Name) + len(r.Scores)
	for _, x := range r.Scores {
		v += x
	}
	return v
}

func mapStructSum(r Catalog) int {
	v := len(r.Name) + len(r.Items)
	for _, it := range r.Items {
		v += it.ID + len(it.Name)
	}
	return v
}

func listWalkSum(doc []byte) int {
	var root RawItems
	if err := json.Unmarshal(doc, &root); err != nil {
		return 0
	}
	v := 0
	for _, raw := range root.Items {
		var it Small
		if err := json.Unmarshal(raw, &it); err != nil {
			v += -1
			continue
		}
		v += it.ID + len(it.Name)
	}
	return v
}

// run decodes doc into a fresh T rounds times and folds each result
// through sum; a failed decode contributes fallback, as the pith `catch`
// arms do.
func run[T any](doc string, rounds, fallback int, sum func(T) int) int {
	b := []byte(doc)
	total := 0
	for i := 0; i < rounds; i++ {
		var r T
		if err := json.Unmarshal(b, &r); err != nil {
			total += fallback
			continue
		}
		total += sum(r)
	}
	return total
}

func main() {
	argAt := func(at int, fallback string) string {
		if len(os.Args) > at {
			return os.Args[at]
		}
		return fallback
	}
	intArg := func(at, fallback int) int {
		if v, err := strconv.Atoi(argAt(at, "")); err == nil && v > 0 {
			return v
		}
		return fallback
	}
	shape := argAt(1, "small")
	size := intArg(2, 8)
	rounds := intArg(3, 20000)
	var total int
	switch shape {
	case "small":
		total = run(smallDoc(size), rounds, -1, smallSum)
	case "wide":
		total = run(wideDoc(size), rounds, -1, wideSum)
	case "nested":
		total = run(nestedDoc(size), rounds, -1, nestedSum)
	case "list":
		total = run(listDoc(size), rounds, 0, listSum)
	case "list_walk":
		doc := []byte(listDoc(size))
		for i := 0; i < rounds; i++ {
			total += listWalkSum(doc)
		}
	case "list_scalar":
		total = run(listScalarDoc(size), rounds, 0, listScalarSum)
	case "nested_list":
		total = run(nestedListDoc(size), rounds, 0, nestedListSum)
	case "map":
		total = run(mapDoc(size), rounds, 0, mapSum)
	case "map_struct":
		total = run(mapStructDoc(size), rounds, 0, mapStructSum)
	case "float":
		total = run(sampleDoc(size, true), rounds, -1, floatSum)
	case "ints":
		total = run(sampleDoc(size, false), rounds, -1, intsSum)
	default:
		fmt.Println("unknown shape: " + shape)
		os.Exit(1)
	}
	fmt.Printf("shape=%s size=%d rounds=%d\n", shape, size, rounds)
	fmt.Printf("checksum=%d\n", total)
}
