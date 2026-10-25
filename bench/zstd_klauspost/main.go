// zstd_codec: go counterpart of bench/zstd_codec.pith, on the pure go
// github.com/klauspost/compress/zstd codec (no cgo). it lives in its own
// module so the rest of bench/ stays free of third-party dependencies.
//
// the same corpus (README.md, docs/*.md, std/*.pith, generated json log
// lines, one run-heavy case), the same 400ms wall-clock budget per phase,
// and the same report lines. go has no c-binding arm here, so each line
// carries only the `pure` column. the encoder runs at SpeedDefault, which
// klauspost documents as roughly zstd level 3, the level the pith kernel
// uses; the decoder reads that frame, since there is no c encoder to make
// the kernel's frame. the label line's ratio and the encode line's frame
// size are klauspost's own.
//
// run it from the repository root, since the corpus paths are relative.
//
// build: (cd bench/zstd_klauspost && go build -o ../zstd_codec_go .)
// run:   ./bench/zstd_codec_go
package main

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/klauspost/compress/zstd"
)

const budget = 400 * time.Millisecond

type entry struct {
	path  string
	isDir bool
}

// pre-order walk in directory-read order (unsorted), the order pith's
// fs.walk uses. filepath.WalkDir sorts, so it would build a different corpus.
func walk(path string, out []entry) []entry {
	info, err := os.Stat(path)
	if err != nil {
		panic(err)
	}
	out = append(out, entry{path, info.IsDir()})
	if info.IsDir() {
		f, err := os.Open(path)
		if err != nil {
			panic(err)
		}
		names, err := f.Readdirnames(-1)
		f.Close()
		if err != nil {
			panic(err)
		}
		for _, name := range names {
			out = walk(filepath.Join(path, name), out)
		}
	}
	return out
}

// concatenate every file under dir ending in suffix, up to limit bytes.
func gather(dir, suffix string, limit int) []byte {
	var buf []byte
	for _, e := range walk(dir, nil) {
		if len(buf) >= limit {
			break
		}
		if e.isDir || !strings.HasSuffix(e.path, suffix) {
			continue
		}
		data, err := os.ReadFile(e.path)
		if err != nil {
			panic(err)
		}
		buf = append(buf, data...)
	}
	if len(buf) > limit {
		buf = buf[:limit]
	}
	return buf
}

// structured log lines: repetitive keys, high-cardinality trace ids.
func synthLogs(count int) []byte {
	levels := []string{"INFO", "WARN", "ERROR", "DEBUG"}
	paths := []string{"/api/v1/users", "/api/v1/orders", "/health", "/metrics"}
	var buf bytes.Buffer
	for i := 0; i < count; i++ {
		trace := i*2654435761 + 1013904223
		fmt.Fprintf(&buf, "{\"ts\":\"2026-08-01T18:%d:%dZ\",\"level\":\"%s\",\"svc\":\"checkout\",\"trace\":\"%d\",\"path\":\"%s\",\"status\":%d,\"ms\":%d}\n",
			i%60, (i*7)%60, levels[i%4], trace, paths[i%4], 200+(i%5)*76, i%250)
	}
	return buf.Bytes()
}

var (
	decoder, _ = zstd.NewReader(nil)
	encoder, _ = zstd.NewWriter(nil, zstd.WithEncoderLevel(zstd.SpeedDefault))
)

func decompress(frame []byte) []byte {
	out, err := decoder.DecodeAll(frame, nil)
	if err != nil {
		panic(err)
	}
	return out
}

func compress(raw []byte) []byte {
	return encoder.EncodeAll(raw, nil)
}

// run f until the budget is spent; returns reps and elapsed milliseconds.
func runFor(f func()) (int, int) {
	reps := 0
	t0 := time.Now()
	for time.Since(t0) < budget {
		f()
		reps++
	}
	return reps, int(time.Since(t0).Milliseconds())
}

func bench(label string, raw []byte) {
	frame := compress(raw)
	if !bytes.Equal(decompress(frame), raw) {
		panic("round trip fails on " + label)
	}

	reps, ms := runFor(func() {
		if len(decompress(frame)) == 0 {
			panic("empty decode")
		}
	})
	encReps, encMs := runFor(func() {
		if len(compress(raw)) == 0 && len(raw) > 0 {
			panic("empty encode")
		}
	})

	// integer math throughout, as in the pith version
	n := len(raw)
	pb := n * reps / (ms + 1)
	epb := n * encReps / (encMs + 1)
	packed := n * 10 / len(frame)
	fmt.Printf("%s: %dKB at %d.%d:1\n", label, n/1024, packed/10, packed%10)
	fmt.Printf("  decode: pure %d MB/s (%d reps)\n", pb*1000/1048576, reps)
	fmt.Printf("  encode: pure %d MB/s (%d reps)   frame %d bytes\n", epb*1000/1048576, encReps, len(frame))
}

func main() {
	readme, err := os.ReadFile("README.md")
	if err != nil {
		panic(err)
	}
	bench("text  ", readme)
	bench("prose ", gather("docs", ".md", 300000))
	bench("source", gather("std", ".pith", 600000))
	bench("json  ", synthLogs(4000))

	// not a realistic workload: a single long run decodes through the
	// match copy and says nothing about entropy decoding.
	runs := make([]byte, 80000)
	for i := range runs {
		runs[i] = byte(97 + (i/4096)%26)
	}
	bench("runs  ", runs)
}
