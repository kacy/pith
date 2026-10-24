// fasthttp counterpart to bench/http_server.pith and bench/http_server.go:
// same routes, same body. it lives in its own module so the rest of bench/
// stays free of third-party dependencies.
//
// build with: (cd bench/fasthttp && go build -o ../http_server_fasthttp .)
package main

import (
	"fmt"
	"os"
	"strconv"
	"strings"

	"github.com/valyala/fasthttp"
)

var kinds = []string{"widget", "gadget", "gizmo", "doodad", "contraption"}

// itemName and itemJSON are copied from bench/http_server.go; keep them in
// step.
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

func handler(ctx *fasthttp.RequestCtx) {
	if !ctx.IsGet() {
		ctx.NotFound()
		return
	}
	switch string(ctx.Path()) {
	case "/item":
		id, _ := strconv.Atoi(string(ctx.QueryArgs().Peek("id")))
		ctx.SetContentType("application/json")
		ctx.WriteString(itemJSON(id))
	case "/health":
		ctx.SetContentType("application/json")
		ctx.WriteString(`{"ok":true}`)
	default:
		ctx.NotFound()
	}
}

func main() {
	port := "8080"
	if len(os.Args) > 1 {
		port = os.Args[1]
	}
	fmt.Println("listening on " + port)
	if err := fasthttp.ListenAndServe(":"+port, handler); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
