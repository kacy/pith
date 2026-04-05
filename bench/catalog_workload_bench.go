package main

import (
	"bufio"
	"fmt"
	"os"
	"os/exec"
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

func main() {
	iterations := 10000
	if len(os.Args) > 1 && os.Args[1] != "" {
		value, err := strconv.Atoi(os.Args[1])
		if err == nil && value > 0 {
			iterations = value
		}
	}

	fmt.Printf("catalog workload comparison (%d iterations)\n", iterations)
	fmt.Println()

	goResult, goOutput, goErr := runWorkload("./bench/catalog_workload_go", iterations)
	if goErr != nil {
		fmt.Println("go workload failed")
		fmt.Println(goOutput)
		fmt.Println(goErr)
		os.Exit(1)
	}

	forgeResult, forgeOutput, forgeErr := runWorkload("./bench/catalog_workload", iterations)
	if forgeErr != nil {
		fmt.Println("forge workload failed")
		fmt.Println(forgeOutput)
		fmt.Println(forgeErr)
		os.Exit(1)
	}

	if goResult.checksum != forgeResult.checksum {
		fmt.Printf("checksum mismatch: go=%d forge=%d\n", goResult.checksum, forgeResult.checksum)
		os.Exit(1)
	}

	fmt.Printf("%-12s %-8s %-8s %-8s\n", "phase", "go", "forge", "ratio")
	fmt.Printf("%-12s %-8d %-8d %-8s\n", "profile", goResult.profileMS, forgeResult.profileMS, ratio(forgeResult.profileMS, goResult.profileMS))
	fmt.Printf("%-12s %-8d %-8d %-8s\n", "search_hot", goResult.searchHotMS, forgeResult.searchHotMS, ratio(forgeResult.searchHotMS, goResult.searchHotMS))
	fmt.Printf("%-12s %-8d %-8d %-8s\n", "search_wide", goResult.searchWideMS, forgeResult.searchWideMS, ratio(forgeResult.searchWideMS, goResult.searchWideMS))
	fmt.Printf("%-12s %-8d %-8d %-8s\n", "batch", goResult.batchMS, forgeResult.batchMS, ratio(forgeResult.batchMS, goResult.batchMS))
	fmt.Printf("%-12s %-8d %-8d %-8s\n", "total", goResult.totalMS, forgeResult.totalMS, ratio(forgeResult.totalMS, goResult.totalMS))
	fmt.Println()
	fmt.Printf("checksum %d\n", goResult.checksum)
}
