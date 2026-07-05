// go counterpart to bench/http_server.pith: same routes, same
// per-request work. build with: go build -o bench/http_server_go bench/http_server.go
package main

import (
	"fmt"
	"net/http"
	"os"
	"strconv"
	"strings"
)

var kinds = []string{"widget", "gadget", "gizmo", "doodad", "contraption"}

func itemName(id int) string {
	return kinds[id%len(kinds)] + "-" + strconv.Itoa(id)
}

func itemJSON(id int) string {
	name := itemName(id)
	price := (id * 37) % 10000
	tags := strings.Split(name, "-")
	var b strings.Builder
	b.WriteString("{\"id\":" + strconv.Itoa(id))
	b.WriteString(",\"name\":\"" + name + "\"")
	b.WriteString(",\"price\":" + strconv.Itoa(price))
	b.WriteString(",\"tags\":[")
	for i, tag := range tags {
		if i > 0 {
			b.WriteString(",")
		}
		b.WriteString("\"" + tag + "\"")
	}
	b.WriteString("]}")
	return b.String()
}

func main() {
	port := "8080"
	if len(os.Args) > 1 {
		port = os.Args[1]
	}
	http.HandleFunc("/item", func(w http.ResponseWriter, r *http.Request) {
		id, _ := strconv.Atoi(r.URL.Query().Get("id"))
		w.Header().Set("Content-Type", "application/json")
		fmt.Fprint(w, itemJSON(id))
	})
	http.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		fmt.Fprint(w, `{"ok":true}`)
	})
	fmt.Println("listening on " + port)
	http.ListenAndServe(":"+port, nil)
}
