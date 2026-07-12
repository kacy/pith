// event_ledger — Go counterpart of bench/event_ledger.pith.
//
// Same deterministic event stream (a shared 31-bit LCG), the same
// aggregation, and the same HMAC-signed canonical summary, so the
// checksum and digest match the Pith, Rust, and Zig versions.
//
// build: go build -o bench/event_ledger_go bench/event_ledger.go
// run:   ./bench/event_ledger_go 200000
package main

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"
)

func lcgNext(state int64) int64 {
	return (state*1103515245 + 12345) % 2147483648
}

func actionName(k int64) string {
	switch k {
	case 0:
		return "view"
	case 1:
		return "click"
	case 2:
		return "buy"
	default:
		return "refund"
	}
}

func regionName(k int64) string {
	switch k {
	case 0:
		return "north"
	case 1:
		return "south"
	case 2:
		return "east"
	default:
		return "west"
	}
}

func generateEvents(count int) string {
	var b strings.Builder
	state := int64(20260711)
	for i := 0; i < count; i++ {
		state = lcgNext(state)
		user := (state / 256) % 1000
		state = lcgNext(state)
		action := actionName((state / 256) % 4)
		state = lcgNext(state)
		amount := (state / 256) % 500
		state = lcgNext(state)
		region := regionName((state / 256) % 4)
		if i > 0 {
			b.WriteByte('\n')
		}
		b.WriteString(`{"id":`)
		b.WriteString(strconv.Itoa(i))
		b.WriteString(`,"user":`)
		b.WriteString(strconv.FormatInt(user, 10))
		b.WriteString(`,"action":"`)
		b.WriteString(action)
		b.WriteString(`","amount":`)
		b.WriteString(strconv.FormatInt(amount, 10))
		b.WriteString(`,"region":"`)
		b.WriteString(region)
		b.WriteString(`"}`)
	}
	return b.String()
}

type event struct {
	User   int    `json:"user"`
	Action string `json:"action"`
	Amount int    `json:"amount"`
	Region string `json:"region"`
}

// parse the stream into an in-memory slice of events with encoding/json.
func parseEvents(stream string) []event {
	lines := strings.Split(stream, "\n")
	events := make([]event, 0, len(lines))
	for _, line := range lines {
		if len(line) == 0 {
			continue
		}
		var e event
		if err := json.Unmarshal([]byte(line), &e); err != nil {
			continue
		}
		events = append(events, e)
	}
	return events
}

type analysis struct {
	regionAmount map[string]int
	actionCount  map[string]int
	uniqueUsers  map[int]struct{}
	highValue    int
	topUser      int
	topUserTotal int
	totalAmount  int
	recordCount  int
}

// the analyze phase: several maps, a set, and a per-user rollup with a
// top-spender scan.
func analyze(events []event) analysis {
	a := analysis{
		regionAmount: make(map[string]int),
		actionCount:  make(map[string]int),
		uniqueUsers:  make(map[int]struct{}),
	}
	userTotal := make(map[int]int)
	// top spender tracked as the per-user totals grow; a user's total only
	// increases, so this equals a scan of the finished map (smallest id
	// wins a tie).
	a.topUser = -1
	a.topUserTotal = -1
	for _, e := range events {
		a.regionAmount[e.Region] += e.Amount
		a.actionCount[e.Action]++
		userTotal[e.User] += e.Amount
		a.uniqueUsers[e.User] = struct{}{}
		if e.Amount >= 400 {
			a.highValue++
		}
		a.totalAmount += e.Amount
		running := userTotal[e.User]
		if running > a.topUserTotal || (running == a.topUserTotal && e.User < a.topUser) {
			a.topUser = e.User
			a.topUserTotal = running
		}
	}
	a.recordCount = len(events)
	return a
}

func buildSummary(a analysis) string {
	var parts []string
	regions := make([]string, 0, len(a.regionAmount))
	for r := range a.regionAmount {
		regions = append(regions, r)
	}
	sort.Strings(regions)
	for _, r := range regions {
		parts = append(parts, "region:"+r+"="+strconv.Itoa(a.regionAmount[r]))
	}
	actions := make([]string, 0, len(a.actionCount))
	for act := range a.actionCount {
		actions = append(actions, act)
	}
	sort.Strings(actions)
	for _, act := range actions {
		parts = append(parts, "action:"+act+"="+strconv.Itoa(a.actionCount[act]))
	}
	parts = append(parts, "users:"+strconv.Itoa(len(a.uniqueUsers)))
	parts = append(parts, "hivalue:"+strconv.Itoa(a.highValue))
	parts = append(parts, "topuser:"+strconv.Itoa(a.topUser)+"="+strconv.Itoa(a.topUserTotal))
	parts = append(parts, "total:"+strconv.Itoa(a.totalAmount))
	parts = append(parts, "records:"+strconv.Itoa(a.recordCount))
	return strings.Join(parts, ";")
}

func digestScore(digest string) int {
	score := 0
	for _, c := range digest {
		score += int(c)
	}
	return score
}

func nowMillis() int64 {
	return time.Now().UnixMilli()
}

func main() {
	events := 200000
	if len(os.Args) > 1 {
		if v, err := strconv.Atoi(os.Args[1]); err == nil && v > 0 {
			events = v
		}
	}

	totalStart := nowMillis()

	start := nowMillis()
	stream := generateEvents(events)
	genMS := nowMillis() - start

	start = nowMillis()
	parsed := parseEvents(stream)
	parseMS := nowMillis() - start

	start = nowMillis()
	a := analyze(parsed)
	analyzeMS := nowMillis() - start

	start = nowMillis()
	summary := buildSummary(a)
	mac := hmac.New(sha256.New, []byte("pith-bench-key"))
	mac.Write([]byte(summary))
	digest := hex.EncodeToString(mac.Sum(nil))
	signMS := nowMillis() - start

	totalMS := nowMillis() - totalStart

	checksum := a.totalAmount + a.recordCount + len(a.uniqueUsers)*31 + a.highValue + a.topUserTotal + digestScore(digest)

	fmt.Println("event ledger benchmark")
	fmt.Printf("events=%d\n", events)
	fmt.Printf("gen_ms=%d\n", genMS)
	fmt.Printf("parse_ms=%d\n", parseMS)
	fmt.Printf("analyze_ms=%d\n", analyzeMS)
	fmt.Printf("sign_ms=%d\n", signMS)
	fmt.Printf("total_ms=%d\n", totalMS)
	fmt.Printf("unique_users=%d\n", len(a.uniqueUsers))
	fmt.Println("digest=" + digest)
	fmt.Printf("checksum=%d\n", checksum)
}
