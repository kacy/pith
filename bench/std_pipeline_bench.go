package main

import (
	"bufio"
	"fmt"
	"os"
	"os/exec"
	"sort"
	"strconv"
	"strings"
)

type pipelineResult struct {
	records     int
	configMS    int
	csvWriteMS  int
	csvReadMS   int
	transformMS int
	jsonMS      int
	gzipHashMS  int
	fsMS        int
	totalMS     int
	checksum    int64
}

func parsePipelineOutput(output string) (pipelineResult, error) {
	result := pipelineResult{}
	scanner := bufio.NewScanner(strings.NewReader(output))
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || !strings.Contains(line, "=") {
			continue
		}
		parts := strings.SplitN(line, "=", 2)
		key := parts[0]
		value := parts[1]
		switch key {
		case "records":
			v, err := strconv.Atoi(value)
			if err != nil {
				return result, err
			}
			result.records = v
		case "config_ms":
			v, err := strconv.Atoi(value)
			if err != nil {
				return result, err
			}
			result.configMS = v
		case "csv_write_ms":
			v, err := strconv.Atoi(value)
			if err != nil {
				return result, err
			}
			result.csvWriteMS = v
		case "csv_read_ms":
			v, err := strconv.Atoi(value)
			if err != nil {
				return result, err
			}
			result.csvReadMS = v
		case "transform_ms":
			v, err := strconv.Atoi(value)
			if err != nil {
				return result, err
			}
			result.transformMS = v
		case "json_ms":
			v, err := strconv.Atoi(value)
			if err != nil {
				return result, err
			}
			result.jsonMS = v
		case "gzip_hash_ms":
			v, err := strconv.Atoi(value)
			if err != nil {
				return result, err
			}
			result.gzipHashMS = v
		case "fs_ms":
			v, err := strconv.Atoi(value)
			if err != nil {
				return result, err
			}
			result.fsMS = v
		case "total_ms":
			v, err := strconv.Atoi(value)
			if err != nil {
				return result, err
			}
			result.totalMS = v
		case "checksum":
			v, err := strconv.ParseInt(value, 10, 64)
			if err != nil {
				return result, err
			}
			result.checksum = v
		}
	}
	return result, scanner.Err()
}

func runWorkload(path string, records int) (pipelineResult, string, error) {
	cmd := exec.Command(path, strconv.Itoa(records))
	out, err := cmd.CombinedOutput()
	text := string(out)
	if err != nil {
		return pipelineResult{}, text, err
	}
	result, parseErr := parsePipelineOutput(text)
	return result, text, parseErr
}

func median(values []int) int {
	sorted := append([]int(nil), values...)
	sort.Ints(sorted)
	return sorted[len(sorted)/2]
}

func medianResult(results []pipelineResult) pipelineResult {
	out := pipelineResult{
		records:  results[0].records,
		checksum: results[0].checksum,
	}
	var config []int
	var csvWrite []int
	var csvRead []int
	var transform []int
	var jsonPhase []int
	var gzipHash []int
	var fsPhase []int
	var total []int
	for _, result := range results {
		config = append(config, result.configMS)
		csvWrite = append(csvWrite, result.csvWriteMS)
		csvRead = append(csvRead, result.csvReadMS)
		transform = append(transform, result.transformMS)
		jsonPhase = append(jsonPhase, result.jsonMS)
		gzipHash = append(gzipHash, result.gzipHashMS)
		fsPhase = append(fsPhase, result.fsMS)
		total = append(total, result.totalMS)
	}
	out.configMS = median(config)
	out.csvWriteMS = median(csvWrite)
	out.csvReadMS = median(csvRead)
	out.transformMS = median(transform)
	out.jsonMS = median(jsonPhase)
	out.gzipHashMS = median(gzipHash)
	out.fsMS = median(fsPhase)
	out.totalMS = median(total)
	return out
}

func runTrials(path string, records, trials int) (pipelineResult, error) {
	results := make([]pipelineResult, 0, trials)
	for i := 0; i < trials; i++ {
		result, output, err := runWorkload(path, records)
		if err != nil {
			return pipelineResult{}, fmt.Errorf("trial %d failed for %s\n%s\n%w", i+1, path, output, err)
		}
		if len(results) > 0 && result.checksum != results[0].checksum {
			return pipelineResult{}, fmt.Errorf("checksum mismatch across trials for %s: %d vs %d", path, results[0].checksum, result.checksum)
		}
		results = append(results, result)
	}
	return medianResult(results), nil
}

func ratio(numerator, denominator int) string {
	if denominator <= 0 {
		return "n/a"
	}
	return fmt.Sprintf("%.2fx", float64(numerator)/float64(denominator))
}

func main() {
	records := 50000
	trials := 5
	if len(os.Args) > 1 && os.Args[1] != "" {
		if value, err := strconv.Atoi(os.Args[1]); err == nil && value > 0 {
			records = value
		}
	}
	if len(os.Args) > 2 && os.Args[2] != "" {
		if value, err := strconv.Atoi(os.Args[2]); err == nil && value > 0 {
			trials = value
		}
	}

	fmt.Printf("std pipeline comparison (%d records, %d trials, median)\n\n", records, trials)
	goResult, goErr := runTrials("./bench/std_pipeline_go", records, trials)
	if goErr != nil {
		fmt.Println(goErr)
		os.Exit(1)
	}
	rustResult, rustErr := runTrials("./bench/std_pipeline_rust/target/release/std_pipeline_rust", records, trials)
	if rustErr != nil {
		fmt.Println(rustErr)
		os.Exit(1)
	}
	forgeResult, forgeErr := runTrials("./bench/std_pipeline", records, trials)
	if forgeErr != nil {
		fmt.Println(forgeErr)
		os.Exit(1)
	}
	if goResult.checksum != rustResult.checksum || goResult.checksum != forgeResult.checksum {
		fmt.Printf("checksum mismatch: go=%d rust=%d forge=%d\n", goResult.checksum, rustResult.checksum, forgeResult.checksum)
		os.Exit(1)
	}

	fmt.Printf("%-14s %-8s %-8s %-8s %-10s %-10s\n", "phase", "go", "rust", "forge", "forge/go", "forge/rust")
	fmt.Printf("%-14s %-8d %-8d %-8d %-10s %-10s\n", "config", goResult.configMS, rustResult.configMS, forgeResult.configMS, ratio(forgeResult.configMS, goResult.configMS), ratio(forgeResult.configMS, rustResult.configMS))
	fmt.Printf("%-14s %-8d %-8d %-8d %-10s %-10s\n", "csv_write", goResult.csvWriteMS, rustResult.csvWriteMS, forgeResult.csvWriteMS, ratio(forgeResult.csvWriteMS, goResult.csvWriteMS), ratio(forgeResult.csvWriteMS, rustResult.csvWriteMS))
	fmt.Printf("%-14s %-8d %-8d %-8d %-10s %-10s\n", "csv_read", goResult.csvReadMS, rustResult.csvReadMS, forgeResult.csvReadMS, ratio(forgeResult.csvReadMS, goResult.csvReadMS), ratio(forgeResult.csvReadMS, rustResult.csvReadMS))
	fmt.Printf("%-14s %-8d %-8d %-8d %-10s %-10s\n", "transform", goResult.transformMS, rustResult.transformMS, forgeResult.transformMS, ratio(forgeResult.transformMS, goResult.transformMS), ratio(forgeResult.transformMS, rustResult.transformMS))
	fmt.Printf("%-14s %-8d %-8d %-8d %-10s %-10s\n", "json", goResult.jsonMS, rustResult.jsonMS, forgeResult.jsonMS, ratio(forgeResult.jsonMS, goResult.jsonMS), ratio(forgeResult.jsonMS, rustResult.jsonMS))
	fmt.Printf("%-14s %-8d %-8d %-8d %-10s %-10s\n", "gzip_hash", goResult.gzipHashMS, rustResult.gzipHashMS, forgeResult.gzipHashMS, ratio(forgeResult.gzipHashMS, goResult.gzipHashMS), ratio(forgeResult.gzipHashMS, rustResult.gzipHashMS))
	fmt.Printf("%-14s %-8d %-8d %-8d %-10s %-10s\n", "fs", goResult.fsMS, rustResult.fsMS, forgeResult.fsMS, ratio(forgeResult.fsMS, goResult.fsMS), ratio(forgeResult.fsMS, rustResult.fsMS))
	fmt.Printf("%-14s %-8d %-8d %-8d %-10s %-10s\n", "total", goResult.totalMS, rustResult.totalMS, forgeResult.totalMS, ratio(forgeResult.totalMS, goResult.totalMS), ratio(forgeResult.totalMS, rustResult.totalMS))
	fmt.Println()
	fmt.Printf("checksum %d\n", goResult.checksum)
}
