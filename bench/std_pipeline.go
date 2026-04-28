package main

import (
	"bytes"
	"compress/gzip"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

type pipelineConfig struct {
	minScore        int
	multiplier      int
	includeInactive bool
}

type pipelineStats struct {
	activeCount int
	selected    int
	scoreSum    int
	quotaSum    int
	urlPathSum  int
	pathPartSum int
	noteHashSum int
}

type reportPayload struct {
	Records     int `json:"records"`
	Active      int `json:"active"`
	Selected    int `json:"selected"`
	ScoreSum    int `json:"score_sum"`
	QuotaSum    int `json:"quota_sum"`
	URLPathSum  int `json:"url_path_sum"`
	PathPartSum int `json:"path_part_sum"`
	NoteHashSum int `json:"note_hash_sum"`
}

func recordsFromArgs() int {
	if len(os.Args) > 1 {
		if v, err := strconv.Atoi(os.Args[1]); err == nil && v > 0 {
			return v
		}
	}
	return 50000
}

func elapsedMS(start time.Time) int {
	return int(time.Since(start).Milliseconds())
}

func regionFor(i int) string {
	switch i % 4 {
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

func activeFor(i int) bool {
	return i%3 != 0
}

func buildConfig() pipelineConfig {
	tomlValues := map[string]string{
		"limits.min_score": "37",
		"output.name":      "report",
	}
	var override map[string]map[string]any
	_ = json.Unmarshal([]byte(`{"limits":{"multiplier":3},"output":{"gzip":true}}`), &override)
	minScore, _ := strconv.Atoi(tomlValues["limits.min_score"])
	multiplier := int(override["limits"]["multiplier"].(float64))
	includeInactive := override["output"]["gzip"].(bool)
	return pipelineConfig{minScore: minScore, multiplier: multiplier, includeInactive: includeInactive}
}

func makeRows(records int) [][]string {
	rows := make([][]string, 0, records+1)
	rows = append(rows, []string{"id", "name", "region", "active", "score", "quota", "url", "joined", "note", "path"})
	for i := 0; i < records; i++ {
		region := regionFor(i)
		score := (i*17 + 11) % 100
		quota := (i*13 + 7) % 50
		active := activeFor(i)
		note := fmt.Sprintf("user %d, region %s", i, region)
		urlText := fmt.Sprintf("https://example.com/api/%s/users/%d?score=%d", region, i, score)
		joined := fmt.Sprintf("2026-%d-%d", (i%12)+1, (i%28)+1)
		userPath := fmt.Sprintf("data//%s/./users/../users/%d.json", region, i)
		rows = append(rows, []string{
			strconv.Itoa(i),
			fmt.Sprintf("user-%d", i),
			region,
			strconv.FormatBool(active),
			strconv.Itoa(score),
			strconv.Itoa(quota),
			urlText,
			joined,
			note,
			userPath,
		})
	}
	return rows
}

func encodeCSVField(field string) string {
	if !strings.ContainsAny(field, "\",\n\r") {
		return field
	}
	var builder strings.Builder
	builder.Grow(len(field) + 2)
	builder.WriteByte('"')
	for _, ch := range field {
		if ch == '"' {
			builder.WriteString("\"\"")
		} else {
			builder.WriteRune(ch)
		}
	}
	builder.WriteByte('"')
	return builder.String()
}

func encodeCSVRow(row []string) string {
	var builder strings.Builder
	for i, field := range row {
		if i > 0 {
			builder.WriteByte(',')
		}
		builder.WriteString(encodeCSVField(field))
	}
	builder.WriteByte('\n')
	return builder.String()
}

func writeCSV(path string, rows [][]string) error {
	var builder strings.Builder
	for _, row := range rows {
		builder.WriteString(encodeCSVRow(row))
	}
	return os.WriteFile(path, []byte(builder.String()), 0o644)
}

func parseCSVRow(line string) []string {
	fields := make([]string, 0, 10)
	var builder strings.Builder
	inQuotes := false
	for i := 0; i < len(line); i++ {
		ch := line[i]
		if inQuotes {
			if ch == '"' {
				if i+1 < len(line) && line[i+1] == '"' {
					builder.WriteByte('"')
					i++
				} else {
					inQuotes = false
				}
			} else {
				builder.WriteByte(ch)
			}
		} else if ch == '"' {
			inQuotes = true
		} else if ch == ',' {
			fields = append(fields, builder.String())
			builder.Reset()
		} else {
			builder.WriteByte(ch)
		}
	}
	fields = append(fields, builder.String())
	return fields
}

func readCSV(path string) ([][]string, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	lines := strings.Split(strings.TrimRight(string(data), "\n"), "\n")
	records := make([][]string, 0, len(lines))
	for _, line := range lines[1:] {
		if strings.TrimSpace(line) == "" {
			continue
		}
		records = append(records, parseCSVRow(strings.TrimRight(line, "\r")))
	}
	return records, nil
}

func fnv1aString(text string) int {
	h := uint32(2166136261)
	for _, b := range []byte(text) {
		h ^= uint32(b)
		h *= 16777619
	}
	return int(h)
}

func transform(rows [][]string, cfg pipelineConfig) pipelineStats {
	var stats pipelineStats
	for _, row := range rows {
		score, _ := strconv.Atoi(row[4])
		quota, _ := strconv.Atoi(row[5])
		active := row[3] == "true"
		parsedURL, _ := url.Parse(row[6])
		cleanPath := filepath.ToSlash(filepath.Clean(row[9]))
		stats.urlPathSum += len(parsedURL.Path)
		if cleanPath != "." {
			stats.pathPartSum += len(strings.Split(cleanPath, "/"))
		}
		stats.noteHashSum += fnv1aString(row[8])
		if active {
			stats.activeCount++
		}
		if (active || cfg.includeInactive) && score >= cfg.minScore {
			stats.selected++
			stats.scoreSum += score * cfg.multiplier
			stats.quotaSum += quota
		}
	}
	return stats
}

func writeReport(stats pipelineStats, records int) ([]byte, error) {
	report := reportPayload{
		Records:     records,
		Active:      stats.activeCount,
		Selected:    stats.selected,
		ScoreSum:    stats.scoreSum,
		QuotaSum:    stats.quotaSum,
		URLPathSum:  stats.urlPathSum,
		PathPartSum: stats.pathPartSum,
		NoteHashSum: stats.noteHashSum,
	}
	return json.Marshal(report)
}

func gzipRoundTrip(data []byte) ([]byte, error) {
	var buf bytes.Buffer
	writer := gzip.NewWriter(&buf)
	if _, err := writer.Write(data); err != nil {
		return nil, err
	}
	if err := writer.Close(); err != nil {
		return nil, err
	}
	reader, err := gzip.NewReader(bytes.NewReader(buf.Bytes()))
	if err != nil {
		return nil, err
	}
	defer reader.Close()
	return io.ReadAll(reader)
}

func digestScore(digest string) int {
	score := 0
	for _, b := range []byte(digest) {
		score += int(b)
	}
	return score
}

func walkScore(root string) (int, error) {
	total := 0
	err := filepath.WalkDir(root, func(path string, entry os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		info, err := entry.Info()
		if err != nil {
			return err
		}
		total += len(entry.Name()) + int(info.Size())
		if entry.IsDir() {
			total += 17
		}
		return nil
	})
	return total, err
}

func printMetric(name string, value int) {
	fmt.Printf("%s=%d\n", name, value)
}

func main() {
	records := recordsFromArgs()
	workDir, err := os.MkdirTemp("", "pith-std-pipeline-")
	if err != nil {
		panic(err)
	}
	defer os.RemoveAll(workDir)
	csvPath := filepath.Join(workDir, "input.csv")
	jsonPath := filepath.Join(workDir, "report.json")
	gzipPath := filepath.Join(workDir, "report.json.gz")
	totalStart := time.Now()

	start := time.Now()
	cfg := buildConfig()
	configMS := elapsedMS(start)

	start = time.Now()
	rows := makeRows(records)
	if err := writeCSV(csvPath, rows); err != nil {
		panic(err)
	}
	csvWriteMS := elapsedMS(start)

	start = time.Now()
	parsedRows, err := readCSV(csvPath)
	if err != nil {
		panic(err)
	}
	csvReadMS := elapsedMS(start)

	start = time.Now()
	stats := transform(parsedRows, cfg)
	transformMS := elapsedMS(start)

	start = time.Now()
	report, err := writeReport(stats, records)
	if err != nil {
		panic(err)
	}
	if err := os.WriteFile(jsonPath, report, 0o644); err != nil {
		panic(err)
	}
	jsonMS := elapsedMS(start)

	start = time.Now()
	decompressed, err := gzipRoundTrip(report)
	if err != nil {
		panic(err)
	}
	compressed := new(bytes.Buffer)
	zw := gzip.NewWriter(compressed)
	_, _ = zw.Write(report)
	_ = zw.Close()
	if err := os.WriteFile(gzipPath, compressed.Bytes(), 0o644); err != nil {
		panic(err)
	}
	sum := sha256.Sum256(decompressed)
	digest := hex.EncodeToString(sum[:])
	gzipHashMS := elapsedMS(start)

	start = time.Now()
	fsScore, err := walkScore(workDir)
	if err != nil {
		panic(err)
	}
	fsMS := elapsedMS(start)

	checksum := stats.scoreSum + stats.quotaSum + stats.urlPathSum + stats.pathPartSum + stats.noteHashSum + digestScore(digest) + len(decompressed) + fsScore*0
	totalMS := elapsedMS(totalStart)

	fmt.Println("std pipeline benchmark")
	printMetric("records", records)
	printMetric("config_ms", configMS)
	printMetric("csv_write_ms", csvWriteMS)
	printMetric("csv_read_ms", csvReadMS)
	printMetric("transform_ms", transformMS)
	printMetric("json_ms", jsonMS)
	printMetric("gzip_hash_ms", gzipHashMS)
	printMetric("fs_ms", fsMS)
	printMetric("total_ms", totalMS)
	printMetric("checksum", checksum)
}
