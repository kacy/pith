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

type workloadResult struct {
	users        int
	iterations   int
	profileMS    int
	searchHotMS  int
	searchWideMS int
	batchMS      int
	totalMS      int
	checksum     int64
}

func parseWorkloadOutput(output string) (workloadResult, error) {
	result := workloadResult{}
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
		case "users":
			v, err := strconv.Atoi(value)
			if err != nil {
				return result, err
			}
			result.users = v
		case "iterations":
			v, err := strconv.Atoi(value)
			if err != nil {
				return result, err
			}
			result.iterations = v
		case "profile_ms":
			v, err := strconv.Atoi(value)
			if err != nil {
				return result, err
			}
			result.profileMS = v
		case "search_hot_ms":
			v, err := strconv.Atoi(value)
			if err != nil {
				return result, err
			}
			result.searchHotMS = v
		case "search_wide_ms":
			v, err := strconv.Atoi(value)
			if err != nil {
				return result, err
			}
			result.searchWideMS = v
		case "batch_ms":
			v, err := strconv.Atoi(value)
			if err != nil {
				return result, err
			}
			result.batchMS = v
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

func runWorkload(path string, iterations int) (workloadResult, string, error) {
	cmd := exec.Command(path, strconv.Itoa(iterations))
	out, err := cmd.CombinedOutput()
	text := string(out)
	if err != nil {
		return workloadResult{}, text, err
	}
	result, parseErr := parseWorkloadOutput(text)
	return result, text, parseErr
}

func ratio(numerator, denominator int) string {
	if denominator <= 0 {
		return "n/a"
	}
	return fmt.Sprintf("%.2fx", float64(numerator)/float64(denominator))
}

func fileExists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}

func median(values []int) int {
	sorted := append([]int(nil), values...)
	sort.Ints(sorted)
	return sorted[len(sorted)/2]
}

func medianResult(results []workloadResult) workloadResult {
	out := workloadResult{
		users:      results[0].users,
		iterations: results[0].iterations,
		checksum:   results[0].checksum,
	}
	var profile []int
	var hot []int
	var wide []int
	var batch []int
	var total []int
	for _, result := range results {
		profile = append(profile, result.profileMS)
		hot = append(hot, result.searchHotMS)
		wide = append(wide, result.searchWideMS)
		batch = append(batch, result.batchMS)
		total = append(total, result.totalMS)
	}
	out.profileMS = median(profile)
	out.searchHotMS = median(hot)
	out.searchWideMS = median(wide)
	out.batchMS = median(batch)
	out.totalMS = median(total)
	return out
}

func runTrials(path string, iterations, trials int) (workloadResult, error) {
	results := make([]workloadResult, 0, trials)
	for i := 0; i < trials; i++ {
		result, output, err := runWorkload(path, iterations)
		if err != nil {
			return workloadResult{}, fmt.Errorf("trial %d failed for %s\n%s\n%w", i+1, path, output, err)
		}
		if len(results) > 0 && result.checksum != results[0].checksum {
			return workloadResult{}, fmt.Errorf("checksum mismatch across trials for %s: %d vs %d", path, results[0].checksum, result.checksum)
		}
		results = append(results, result)
	}
	return medianResult(results), nil
}

func main() {
	iterations := 10000
	trials := 5
	if len(os.Args) > 1 && os.Args[1] != "" {
		value, err := strconv.Atoi(os.Args[1])
		if err == nil && value > 0 {
			iterations = value
		}
	}
	if len(os.Args) > 2 && os.Args[2] != "" {
		value, err := strconv.Atoi(os.Args[2])
		if err == nil && value > 0 {
			trials = value
		}
	}

	fmt.Printf("catalog workload comparison (%d iterations, %d trials, median)\n", iterations, trials)
	fmt.Println()

	goResult, goErr := runTrials("./bench/catalog_workload_go", iterations, trials)
	if goErr != nil {
		fmt.Println(goErr)
		os.Exit(1)
	}

	pithResult, pithErr := runTrials("./bench/catalog_workload", iterations, trials)
	if pithErr != nil {
		fmt.Println(pithErr)
		os.Exit(1)
	}

	if goResult.checksum != pithResult.checksum {
		fmt.Printf("checksum mismatch: go=%d pith=%d\n", goResult.checksum, pithResult.checksum)
		os.Exit(1)
	}

	if fileExists("./bench/catalog_workload_rust") {
		rustResult, rustErr := runTrials("./bench/catalog_workload_rust", iterations, trials)
		if rustErr != nil {
			fmt.Println(rustErr)
			os.Exit(1)
		}
		if goResult.checksum != rustResult.checksum {
			fmt.Printf("checksum mismatch: go=%d rust=%d\n", goResult.checksum, rustResult.checksum)
			os.Exit(1)
		}

		fmt.Printf("%-12s %-8s %-8s %-8s %-10s %-10s\n", "phase", "go", "rust", "pith", "pith/go", "pith/rust")
		fmt.Printf("%-12s %-8d %-8d %-8d %-10s %-10s\n", "profile", goResult.profileMS, rustResult.profileMS, pithResult.profileMS, ratio(pithResult.profileMS, goResult.profileMS), ratio(pithResult.profileMS, rustResult.profileMS))
		fmt.Printf("%-12s %-8d %-8d %-8d %-10s %-10s\n", "search_hot", goResult.searchHotMS, rustResult.searchHotMS, pithResult.searchHotMS, ratio(pithResult.searchHotMS, goResult.searchHotMS), ratio(pithResult.searchHotMS, rustResult.searchHotMS))
		fmt.Printf("%-12s %-8d %-8d %-8d %-10s %-10s\n", "search_wide", goResult.searchWideMS, rustResult.searchWideMS, pithResult.searchWideMS, ratio(pithResult.searchWideMS, goResult.searchWideMS), ratio(pithResult.searchWideMS, rustResult.searchWideMS))
		fmt.Printf("%-12s %-8d %-8d %-8d %-10s %-10s\n", "batch", goResult.batchMS, rustResult.batchMS, pithResult.batchMS, ratio(pithResult.batchMS, goResult.batchMS), ratio(pithResult.batchMS, rustResult.batchMS))
		fmt.Printf("%-12s %-8d %-8d %-8d %-10s %-10s\n", "total", goResult.totalMS, rustResult.totalMS, pithResult.totalMS, ratio(pithResult.totalMS, goResult.totalMS), ratio(pithResult.totalMS, rustResult.totalMS))
	} else {
		fmt.Printf("%-12s %-8s %-8s %-8s\n", "phase", "go", "pith", "ratio")
		fmt.Printf("%-12s %-8d %-8d %-8s\n", "profile", goResult.profileMS, pithResult.profileMS, ratio(pithResult.profileMS, goResult.profileMS))
		fmt.Printf("%-12s %-8d %-8d %-8s\n", "search_hot", goResult.searchHotMS, pithResult.searchHotMS, ratio(pithResult.searchHotMS, goResult.searchHotMS))
		fmt.Printf("%-12s %-8d %-8d %-8s\n", "search_wide", goResult.searchWideMS, pithResult.searchWideMS, ratio(pithResult.searchWideMS, goResult.searchWideMS))
		fmt.Printf("%-12s %-8d %-8d %-8s\n", "batch", goResult.batchMS, pithResult.batchMS, ratio(pithResult.batchMS, goResult.batchMS))
		fmt.Printf("%-12s %-8d %-8d %-8s\n", "total", goResult.totalMS, pithResult.totalMS, ratio(pithResult.totalMS, goResult.totalMS))
	}
	fmt.Println()
	fmt.Printf("checksum %d\n", goResult.checksum)
}
