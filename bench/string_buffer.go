// string_buffer: go counterpart of bench/string_buffer.pith.
//
// two arms over the same inputs. "current" is strings.Builder, the buffer a
// go developer reaches for. "legacy" mirrors the chunk-list storage pith's
// std.io used to have: every write appends to a []string, and once the list
// holds 32 chunks it is joined back into one. the pith legacy arm also looks
// its list up in per-thread maps keyed by a handle on every write; that
// indirection is pith-specific and is left out here.
//
// strings.Builder.String returns the built bytes without copying them, so a
// snapshot here costs nothing per byte, where pith's string() copies the
// prefix. that is a property of the go type, and the snapshot workload shows
// it.
//
// inputs come from the same seeded 31-bit lcg and are built before the
// clock starts. the output line and checksum match the pith and rust
// versions for the same arguments (total_us, per_op_us and peak_kb aside).
//
// build: go build -o bench/string_buffer_go bench/string_buffer.go
// run:   ./bench/string_buffer_go <arm> <workload> <n> <chunk> <rounds> [k]
//        ./bench/string_buffer_go current build 100000 200 20
package main

import (
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"
)

const mod = 1000000007

type legacyBuffer struct {
	parts  []string
	length int
}

func (b *legacyBuffer) string() string {
	switch len(b.parts) {
	case 0:
		return ""
	case 1:
		return b.parts[0]
	}
	return strings.Join(b.parts, "")
}

func (b *legacyBuffer) write(data string) {
	if len(data) == 0 {
		return
	}
	b.parts = append(b.parts, data)
	b.length += len(data)
	if len(b.parts) >= 32 {
		joined := strings.Join(b.parts, "")
		b.parts = []string{joined}
		b.length = len(joined)
	}
}

func chunksFor(n, chunk int) []string {
	out := make([]string, 0, n)
	state := 2463534242
	for i := 0; i < n; i++ {
		piece := make([]byte, 0, chunk)
		for j := 0; j < chunk; j++ {
			state = (state*1103515245 + 12345) % 2147483648
			piece = append(piece, byte(97+state%26))
		}
		out = append(out, string(piece))
	}
	return out
}

func foldSnapshot(sum int, snap string) int {
	last := 0
	if len(snap) > 0 {
		last = int(snap[len(snap)-1])
	}
	return (sum*31 + len(snap)*7 + last) % mod
}

func legacyBuild(chunks []string) string {
	var buf legacyBuffer
	for _, piece := range chunks {
		buf.write(piece)
	}
	return buf.string()
}

func currentBuild(chunks []string) string {
	var buf strings.Builder
	for _, piece := range chunks {
		buf.WriteString(piece)
	}
	return buf.String()
}

func legacySnapshot(chunks []string, every int) int {
	var buf legacyBuffer
	sum := 0
	for i, piece := range chunks {
		buf.write(piece)
		if (i+1)%every == 0 {
			sum = foldSnapshot(sum, buf.string())
		}
	}
	return foldSnapshot(sum, buf.string())
}

func currentSnapshot(chunks []string, every int) int {
	var buf strings.Builder
	sum := 0
	for i, piece := range chunks {
		buf.WriteString(piece)
		if (i+1)%every == 0 {
			sum = foldSnapshot(sum, buf.String())
		}
	}
	return foldSnapshot(sum, buf.String())
}

func fnv1a(s string) int {
	h := uint32(2166136261)
	for i := 0; i < len(s); i++ {
		h = (h ^ uint32(s[i])) * 16777619
	}
	return int(h)
}

// the process high-water resident set in kilobytes.
func peakKB() int {
	status, err := os.ReadFile("/proc/self/status")
	if err != nil {
		return 0
	}
	for _, line := range strings.Split(string(status), "\n") {
		if strings.HasPrefix(line, "VmHWM:") {
			digits := strings.TrimSpace(strings.TrimSuffix(strings.TrimPrefix(line, "VmHWM:"), "kB"))
			if v, err := strconv.Atoi(digits); err == nil {
				return v
			}
		}
	}
	return 0
}

func argAt(at int, fallback string) string {
	if len(os.Args) > at {
		return os.Args[at]
	}
	return fallback
}

func intArg(at int, fallback int) int {
	if len(os.Args) > at {
		if v, err := strconv.Atoi(os.Args[at]); err == nil {
			return v
		}
	}
	return fallback
}

func main() {
	arm := argAt(1, "current")
	workload := argAt(2, "build")
	n := intArg(3, 10000)
	chunk := intArg(4, 8)
	rounds := intArg(5, 1)
	every := intArg(6, 100)
	if every <= 0 {
		every = 1
	}

	chunks := chunksFor(n, chunk)

	sum := 0
	lastOut := ""
	t0 := time.Now()
	for round := 0; round < rounds; round++ {
		if workload == "snapshot" {
			if arm == "legacy" {
				sum = (sum + legacySnapshot(chunks, every)) % mod
			} else {
				sum = (sum + currentSnapshot(chunks, every)) % mod
			}
		} else {
			if arm == "legacy" {
				lastOut = legacyBuild(chunks)
			} else {
				lastOut = currentBuild(chunks)
			}
			sum = foldSnapshot(sum, lastOut)
		}
	}
	elapsedUs := time.Since(t0).Microseconds()
	sum = (sum + fnv1a(lastOut)%mod) % mod

	perOpUs := elapsedUs
	if rounds > 0 {
		perOpUs = elapsedUs / int64(rounds)
	}
	fmt.Printf("%s %s n=%d chunk=%d rounds=%d every=%d total_us=%d per_op_us=%d peak_kb=%d checksum=%d\n",
		arm, workload, n, chunk, rounds, every, elapsedUs, perOpUs, peakKB(), sum)
}
