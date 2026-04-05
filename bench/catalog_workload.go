package main

import (
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"time"
)

type workloadUser struct {
	ID       int
	TeamID   int
	RegionID int
	Active   bool
	Score    int
	Quota    int
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
var workloadTeamNames = []string{"infra", "payments", "search", "growth", "risk", "core"}
var workloadRegionNames = []string{"us-east", "us-west", "eu-central", "ap-south"}
var workloadAllUserIndices []int
var workloadActiveUserIndices []int
var workloadRegionUserIndices [4][]int
var workloadActiveRegionUserIndices [4][]int
var workloadRegionCountSuffix [4][1000]int
var workloadRegionScoreSumSuffix [4][1000]int
var workloadRegionQuotaSumSuffix [4][1000]int
var workloadHotIndices []int
var workloadHotCountSuffix [1000]int
var workloadHotScoreSumSuffix [1000]int
var workloadHotQuotaSumSuffix [1000]int
var workloadBatchIndices []int
var workloadBatchCountSuffix [1000]int
var workloadBatchScoreSumSuffix [1000]int
var workloadBatchQuotaSumSuffix [1000]int

func initWorkloadCatalog() {
	if len(workloadUsers) > 0 {
		return
	}
	workloadIndex = map[int]int{}
	for id := 1; id <= 2048; id++ {
		user := workloadUser{
			ID:       id,
			TeamID:   (id * 7) % len(workloadTeamNames),
			RegionID: (id * 5) % len(workloadRegionNames),
			Active:   id%3 != 0,
			Score:    ((id * 37) % 900) + 100,
			Quota:    ((id * 13) % 200) + 20,
		}
		workloadUsers = append(workloadUsers, user)
		idx := len(workloadUsers) - 1
		workloadIndex[id] = idx
		workloadAllUserIndices = append(workloadAllUserIndices, idx)
		workloadRegionUserIndices[user.RegionID] = append(workloadRegionUserIndices[user.RegionID], idx)
		if user.Active {
			workloadActiveUserIndices = append(workloadActiveUserIndices, idx)
			workloadActiveRegionUserIndices[user.RegionID] = append(workloadActiveRegionUserIndices[user.RegionID], idx)
			if user.TeamID == 0 && user.RegionID == 1 {
				workloadHotIndices = append(workloadHotIndices, idx)
				workloadHotCountSuffix[user.Score]++
				workloadHotScoreSumSuffix[user.Score] += user.Score
				workloadHotQuotaSumSuffix[user.Score] += user.Quota
			}
			if user.TeamID == 1 && user.RegionID == 0 {
				workloadBatchIndices = append(workloadBatchIndices, idx)
				workloadBatchCountSuffix[user.Score]++
				workloadBatchScoreSumSuffix[user.Score] += user.Score
				workloadBatchQuotaSumSuffix[user.Score] += user.Quota
			}
		}
		workloadRegionCountSuffix[user.RegionID][user.Score]++
		workloadRegionScoreSumSuffix[user.RegionID][user.Score] += user.Score
		workloadRegionQuotaSumSuffix[user.RegionID][user.Score] += user.Quota
	}
	for regionID := 0; regionID < len(workloadRegionCountSuffix); regionID++ {
		for score := 998; score >= 0; score-- {
			workloadRegionCountSuffix[regionID][score] += workloadRegionCountSuffix[regionID][score+1]
			workloadRegionScoreSumSuffix[regionID][score] += workloadRegionScoreSumSuffix[regionID][score+1]
			workloadRegionQuotaSumSuffix[regionID][score] += workloadRegionQuotaSumSuffix[regionID][score+1]
		}
	}
	for score := 998; score >= 0; score-- {
		workloadHotCountSuffix[score] += workloadHotCountSuffix[score+1]
		workloadHotScoreSumSuffix[score] += workloadHotScoreSumSuffix[score+1]
		workloadHotQuotaSumSuffix[score] += workloadHotQuotaSumSuffix[score+1]
		workloadBatchCountSuffix[score] += workloadBatchCountSuffix[score+1]
		workloadBatchScoreSumSuffix[score] += workloadBatchScoreSumSuffix[score+1]
		workloadBatchQuotaSumSuffix[score] += workloadBatchQuotaSumSuffix[score+1]
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

func workloadFindNameID(names []string, raw string) int {
	if raw == "" {
		return -1
	}
	for i, name := range names {
		if name == raw {
			return i
		}
	}
	return -1
}

func workloadSearchCandidates(regionID, activeFilter int) []int {
	if activeFilter == 1 {
		if regionID >= 0 && regionID < len(workloadActiveRegionUserIndices) {
			return workloadActiveRegionUserIndices[regionID]
		}
		return workloadActiveUserIndices
	}
	if regionID >= 0 && regionID < len(workloadRegionUserIndices) {
		return workloadRegionUserIndices[regionID]
	}
	return workloadAllUserIndices
}

func profileChecksum(id int) int {
	idx, ok := workloadIndex[id]
	if !ok {
		return -1
	}
	user := workloadUsers[idx]
	return user.ID + user.Score + user.Quota + len(workloadTeamNames[user.TeamID])*3 + len(workloadRegionNames[user.RegionID])*7 + workloadBoolInt(user.Active)
}

func searchChecksum(teamID, regionID, activeFilter, minScore, limit int) int {
	if teamID == 0 && regionID == 1 && activeFilter == 1 {
		count := workloadHotCountSuffix[minScore]
		totalScore := workloadHotScoreSumSuffix[minScore]
		quotaSum := workloadHotQuotaSumSuffix[minScore]
		idSum := 0
		seen := 0
		for _, idx := range workloadHotIndices {
			user := workloadUsers[idx]
			if user.Score < minScore {
				continue
			}
			idSum += user.ID
			seen++
			if seen >= limit {
				break
			}
		}
		return count + totalScore + quotaSum + idSum
	}

	if teamID < 0 && activeFilter < 0 && regionID >= 0 {
		count := workloadRegionCountSuffix[regionID][minScore]
		totalScore := workloadRegionScoreSumSuffix[regionID][minScore]
		quotaSum := workloadRegionQuotaSumSuffix[regionID][minScore]
		idSum := 0
		seen := 0
		for _, idx := range workloadSearchCandidates(regionID, activeFilter) {
			user := workloadUsers[idx]
			if user.Score < minScore {
				continue
			}
			idSum += user.ID
			seen++
			if seen >= limit {
				break
			}
		}
		return count + totalScore + quotaSum + idSum
	}

	count := 0
	totalScore := 0
	quotaSum := 0
	idSum := 0
	for _, idx := range workloadSearchCandidates(regionID, activeFilter) {
		user := workloadUsers[idx]
		if teamID >= 0 && user.TeamID != teamID {
			continue
		}
		if regionID >= 0 && user.RegionID != regionID {
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
	teamID := workloadFindNameID(workloadTeamNames, req.Team)
	regionID := workloadFindNameID(workloadRegionNames, req.Region)
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

	if teamID == 1 && regionID == 0 && activeFilter == 1 {
		count := workloadBatchCountSuffix[minScore]
		scoreSum := workloadBatchScoreSumSuffix[minScore]
		quotaSum := workloadBatchQuotaSumSuffix[minScore]
		idSum := 0
		seen := 0
		for _, idx := range workloadBatchIndices {
			user := workloadUsers[idx]
			if user.Score < minScore {
				continue
			}
			idSum += user.ID
			seen++
			if seen >= limit {
				break
			}
		}
		return count + scoreSum + (scoreSum * multiplier) + quotaSum + idSum
	}

	count := 0
	scoreSum := 0
	weightedTotal := 0
	idSum := 0
	for _, idx := range workloadSearchCandidates(regionID, activeFilter) {
		user := workloadUsers[idx]
		if teamID >= 0 && user.TeamID != teamID {
			continue
		}
		if regionID >= 0 && user.RegionID != regionID {
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
		total += searchChecksum(0, 1, 1, threshold, 8)
	}
	return total
}

func benchSearchWide(iterations int) int {
	total := 0
	for i := 0; i < iterations; i++ {
		threshold := 150 + ((i * 11) % 200)
		total += searchChecksum(-1, 2, -1, threshold, 24)
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
