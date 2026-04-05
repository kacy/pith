package main

import (
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"time"
)

type workloadUser struct {
	ID     int
	Team   string
	Region string
	Active bool
	Score  int
	Quota  int
}

type workloadBatchRequest struct {
	Team       string `json:"team"`
	Region     string `json:"region"`
	Active     string `json:"active"`
	MinScore   int    `json:"min_score"`
	Limit      int    `json:"limit"`
	Multiplier int    `json:"multiplier"`
}

var workloadUsers []workloadUser
var workloadIndex map[int]int

func initWorkloadCatalog() {
	if len(workloadUsers) > 0 {
		return
	}
	teams := []string{"infra", "payments", "search", "growth", "risk", "core"}
	regions := []string{"us-east", "us-west", "eu-central", "ap-south"}
	workloadIndex = map[int]int{}
	for id := 1; id <= 2048; id++ {
		user := workloadUser{
			ID:     id,
			Team:   teams[(id*7)%len(teams)],
			Region: regions[(id*5)%len(regions)],
			Active: id%3 != 0,
			Score:  ((id * 37) % 900) + 100,
			Quota:  ((id * 13) % 200) + 20,
		}
		workloadUsers = append(workloadUsers, user)
		workloadIndex[id] = len(workloadUsers) - 1
	}
}

func workloadIterations() int {
	if len(os.Args) < 2 || os.Args[1] == "" {
		return 4000
	}
	value, err := strconv.Atoi(os.Args[1])
	if err != nil || value <= 0 {
		return 4000
	}
	return value
}

func workloadBoolInt(value bool) int {
	if value {
		return 1
	}
	return 0
}

func workloadParseActive(raw string) int {
	if raw == "1" || raw == "true" {
		return 1
	}
	if raw == "0" || raw == "false" {
		return 0
	}
	return -1
}

func profileChecksum(id int) int {
	idx, ok := workloadIndex[id]
	if !ok {
		return -1
	}
	user := workloadUsers[idx]
	return user.ID + user.Score + user.Quota + len(user.Team)*3 + len(user.Region)*7 + workloadBoolInt(user.Active)
}

func searchChecksum(team, region string, activeFilter, minScore, limit int) int {
	count := 0
	totalScore := 0
	quotaSum := 0
	idSum := 0
	for _, user := range workloadUsers {
		if team != "" && user.Team != team {
			continue
		}
		if region != "" && user.Region != region {
			continue
		}
		if activeFilter == 1 && !user.Active {
			continue
		}
		if activeFilter == 0 && user.Active {
			continue
		}
		if user.Score < minScore {
			continue
		}
		count++
		totalScore += user.Score
		quotaSum += user.Quota
		if count <= limit {
			idSum += user.ID
		}
	}
	return count + totalScore + quotaSum + idSum
}

func batchChecksum(body string) int {
	var req workloadBatchRequest
	if err := json.Unmarshal([]byte(body), &req); err != nil {
		return -1
	}
	minScore := req.MinScore
	if minScore < 0 {
		minScore = 0
	}
	limit := req.Limit
	if limit <= 0 {
		limit = 10
	}
	multiplier := req.Multiplier
	if multiplier <= 0 {
		multiplier = 3
	}
	activeFilter := workloadParseActive(req.Active)
	count := 0
	scoreSum := 0
	weightedTotal := 0
	idSum := 0
	for _, user := range workloadUsers {
		if req.Team != "" && user.Team != req.Team {
			continue
		}
		if req.Region != "" && user.Region != req.Region {
			continue
		}
		if activeFilter == 1 && !user.Active {
			continue
		}
		if activeFilter == 0 && user.Active {
			continue
		}
		if user.Score < minScore {
			continue
		}
		count++
		scoreSum += user.Score
		weightedTotal += user.Score*multiplier + user.Quota
		if count <= limit {
			idSum += user.ID
		}
	}
	return count + scoreSum + weightedTotal + idSum
}

func benchProfile(iterations int) int {
	total := 0
	for i := 0; i < iterations; i++ {
		id := ((i * 17) % 2048) + 1
		total += profileChecksum(id)
	}
	return total
}

func benchSearchHot(iterations int) int {
	total := 0
	for i := 0; i < iterations; i++ {
		threshold := 300 + ((i * 29) % 350)
		total += searchChecksum("infra", "us-west", 1, threshold, 8)
	}
	return total
}

func benchSearchWide(iterations int) int {
	total := 0
	for i := 0; i < iterations; i++ {
		threshold := 150 + ((i * 11) % 200)
		total += searchChecksum("", "eu-central", -1, threshold, 24)
	}
	return total
}

func benchBatch(iterations int) int {
	payload := `{"team":"payments","region":"us-east","active":"1","min_score":500,"limit":12,"multiplier":4}`
	total := 0
	for i := 0; i < iterations; i++ {
		total += batchChecksum(payload)
	}
	return total
}

func main() {
	initWorkloadCatalog()
	iterations := workloadIterations()

	fmt.Println("catalog workload benchmark")
	fmt.Printf("users=%d\n", len(workloadUsers))
	fmt.Printf("iterations=%d\n", iterations)

	totalStart := nowMillis()

	t0 := nowMillis()
	profileTotal := benchProfile(iterations)
	profileMS := nowMillis() - t0

	t1 := nowMillis()
	hotTotal := benchSearchHot(iterations)
	hotMS := nowMillis() - t1

	t2 := nowMillis()
	wideTotal := benchSearchWide(iterations)
	wideMS := nowMillis() - t2

	t3 := nowMillis()
	batchTotal := benchBatch(iterations)
	batchMS := nowMillis() - t3

	totalMS := nowMillis() - totalStart
	checksum := profileTotal + hotTotal + wideTotal + batchTotal

	fmt.Printf("profile_ms=%d\n", profileMS)
	fmt.Printf("search_hot_ms=%d\n", hotMS)
	fmt.Printf("search_wide_ms=%d\n", wideMS)
	fmt.Printf("batch_ms=%d\n", batchMS)
	fmt.Printf("total_ms=%d\n", totalMS)
	fmt.Printf("checksum=%d\n", checksum)
}

func nowMillis() int64 {
	return time.Now().UnixMilli()
}
