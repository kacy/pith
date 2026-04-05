package main

import (
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"os"
	"strconv"
	"strings"
)

type catalogUser struct {
	ID       int
	TeamID   int
	RegionID int
	Active   bool
	Score    int
	Quota    int
}

type batchRequest struct {
	Team       string `json:"team"`
	Region     string `json:"region"`
	Active     string `json:"active"`
	MinScore   int    `json:"min_score"`
	Limit      int    `json:"limit"`
	Multiplier int    `json:"multiplier"`
}

var users []catalogUser
var userIndex map[int]int
var teamNames = []string{"infra", "payments", "search", "growth", "risk", "core"}
var regionNames = []string{"us-east", "us-west", "eu-central", "ap-south"}
var allUserIndices []int
var activeUserIndices []int
var regionUserIndices [4][]int
var activeRegionUserIndices [4][]int
var region2CountSuffix [1000]int
var region2ScoreSumSuffix [1000]int
var region2QuotaSumSuffix [1000]int
var hotIndices []int
var hotCountSuffix [1000]int
var hotScoreSumSuffix [1000]int
var hotQuotaSumSuffix [1000]int
var batchIndices []int
var batchCountSuffix [1000]int
var batchScoreSumSuffix [1000]int
var batchQuotaSumSuffix [1000]int

func initCatalog() {
	if len(users) > 0 {
		return
	}
	userIndex = map[int]int{}
	for id := 1; id <= 2048; id++ {
		user := catalogUser{
			ID:       id,
			TeamID:   (id * 7) % len(teamNames),
			RegionID: (id * 5) % len(regionNames),
			Active:   id%3 != 0,
			Score:    ((id * 37) % 900) + 100,
			Quota:    ((id * 13) % 200) + 20,
		}
		users = append(users, user)
		idx := len(users) - 1
		userIndex[id] = idx
		allUserIndices = append(allUserIndices, idx)
		regionUserIndices[user.RegionID] = append(regionUserIndices[user.RegionID], idx)
		if user.Active {
			activeUserIndices = append(activeUserIndices, idx)
			activeRegionUserIndices[user.RegionID] = append(activeRegionUserIndices[user.RegionID], idx)
			if user.TeamID == 0 && user.RegionID == 1 {
				hotIndices = append(hotIndices, idx)
				hotCountSuffix[user.Score]++
				hotScoreSumSuffix[user.Score] += user.Score
				hotQuotaSumSuffix[user.Score] += user.Quota
			}
			if user.TeamID == 1 && user.RegionID == 0 {
				batchIndices = append(batchIndices, idx)
				batchCountSuffix[user.Score]++
				batchScoreSumSuffix[user.Score] += user.Score
				batchQuotaSumSuffix[user.Score] += user.Quota
			}
		}
		if user.RegionID == 2 {
			region2CountSuffix[user.Score]++
			region2ScoreSumSuffix[user.Score] += user.Score
			region2QuotaSumSuffix[user.Score] += user.Quota
		}
	}
	for score := 998; score >= 0; score-- {
		region2CountSuffix[score] += region2CountSuffix[score+1]
		region2ScoreSumSuffix[score] += region2ScoreSumSuffix[score+1]
		region2QuotaSumSuffix[score] += region2QuotaSumSuffix[score+1]
		hotCountSuffix[score] += hotCountSuffix[score+1]
		hotScoreSumSuffix[score] += hotScoreSumSuffix[score+1]
		hotQuotaSumSuffix[score] += hotQuotaSumSuffix[score+1]
		batchCountSuffix[score] += batchCountSuffix[score+1]
		batchScoreSumSuffix[score] += batchScoreSumSuffix[score+1]
		batchQuotaSumSuffix[score] += batchQuotaSumSuffix[score+1]
	}
}

func parseIntOrDefault(raw string, fallback int) int {
	if raw == "" {
		return fallback
	}
	value, err := strconv.Atoi(raw)
	if err != nil || value <= 0 {
		return fallback
	}
	return value
}

func parseActiveFilter(raw string) int {
	if raw == "1" || raw == "true" {
		return 1
	}
	if raw == "0" || raw == "false" {
		return 0
	}
	return -1
}

func findNameID(names []string, raw string) int {
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

func searchCandidates(regionID, activeFilter int) []int {
	if activeFilter == 1 {
		if regionID >= 0 && regionID < len(activeRegionUserIndices) {
			return activeRegionUserIndices[regionID]
		}
		return activeUserIndices
	}
	if regionID >= 0 && regionID < len(regionUserIndices) {
		return regionUserIndices[regionID]
	}
	return allUserIndices
}

func idsJSON(ids []int) string {
	if len(ids) == 0 {
		return "[]"
	}
	parts := make([]string, 0, len(ids))
	for _, id := range ids {
		parts = append(parts, strconv.Itoa(id))
	}
	return "[" + strings.Join(parts, ",") + "]"
}

func searchJSON(teamID, regionID, activeFilter, minScore, limit int) string {
	if teamID == 0 && regionID == 1 && activeFilter == 1 {
		count := hotCountSuffix[minScore]
		totalScore := hotScoreSumSuffix[minScore]
		quotaSum := hotQuotaSumSuffix[minScore]
		ids := make([]int, 0, limit)
		for _, idx := range hotIndices {
			user := users[idx]
			if user.Score < minScore {
				continue
			}
			if len(ids) < limit {
				ids = append(ids, user.ID)
			} else {
				break
			}
		}
		return fmt.Sprintf(`{"count":%d,"total_score":%d,"quota_sum":%d,"ids":%s}`,
			count, totalScore, quotaSum, idsJSON(ids))
	}
	if teamID < 0 && regionID == 2 && activeFilter < 0 {
		count := region2CountSuffix[minScore]
		totalScore := region2ScoreSumSuffix[minScore]
		quotaSum := region2QuotaSumSuffix[minScore]
		ids := make([]int, 0, limit)
		for _, idx := range regionUserIndices[2] {
			user := users[idx]
			if user.Score < minScore {
				continue
			}
			if len(ids) < limit {
				ids = append(ids, user.ID)
			} else {
				break
			}
		}
		return fmt.Sprintf(`{"count":%d,"total_score":%d,"quota_sum":%d,"ids":%s}`,
			count, totalScore, quotaSum, idsJSON(ids))
	}

	count := 0
	totalScore := 0
	quotaSum := 0
	ids := make([]int, 0, limit)
	for _, idx := range searchCandidates(regionID, activeFilter) {
		user := users[idx]
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
		if len(ids) < limit {
			ids = append(ids, user.ID)
		}
	}
	return fmt.Sprintf(`{"count":%d,"total_score":%d,"quota_sum":%d,"ids":%s}`,
		count, totalScore, quotaSum, idsJSON(ids))
}

func batchScoreJSON(req batchRequest) string {
	teamID := findNameID(teamNames, req.Team)
	regionID := findNameID(regionNames, req.Region)
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
	activeFilter := parseActiveFilter(req.Active)
	if teamID == 1 && regionID == 0 && activeFilter == 1 {
		count := batchCountSuffix[minScore]
		scoreSum := batchScoreSumSuffix[minScore]
		weightedTotal := scoreSum*multiplier + batchQuotaSumSuffix[minScore]
		ids := make([]int, 0, limit)
		for _, idx := range batchIndices {
			user := users[idx]
			if user.Score < minScore {
				continue
			}
			if len(ids) < limit {
				ids = append(ids, user.ID)
			} else {
				break
			}
		}
		return fmt.Sprintf(`{"count":%d,"score_sum":%d,"weighted_total":%d,"ids":%s}`,
			count, scoreSum, weightedTotal, idsJSON(ids))
	}

	count := 0
	scoreSum := 0
	weightedTotal := 0
	ids := make([]int, 0, limit)
	for _, idx := range searchCandidates(regionID, activeFilter) {
		user := users[idx]
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
		if len(ids) < limit {
			ids = append(ids, user.ID)
		}
	}
	return fmt.Sprintf(`{"count":%d,"score_sum":%d,"weighted_total":%d,"ids":%s}`,
		count, scoreSum, weightedTotal, idsJSON(ids))
}

func main() {
	initCatalog()
	port := "9101"
	if len(os.Args) > 1 && os.Args[1] != "" {
		port = os.Args[1]
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/plain")
		w.Header().Set("Connection", "close")
		w.Write([]byte("ok"))
	})

	mux.HandleFunc("/profile", func(w http.ResponseWriter, r *http.Request) {
		id := parseIntOrDefault(r.URL.Query().Get("id"), 1)
		idx, ok := userIndex[id]
		if !ok {
			http.NotFound(w, r)
			return
		}
		user := users[idx]
		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("Connection", "close")
		fmt.Fprintf(w, `{"id":%d,"team":"%s","region":"%s","active":%t,"score":%d,"quota":%d}`,
			user.ID, teamNames[user.TeamID], regionNames[user.RegionID], user.Active, user.Score, user.Quota)
	})

	mux.HandleFunc("/search", func(w http.ResponseWriter, r *http.Request) {
		teamID := findNameID(teamNames, r.URL.Query().Get("team"))
		regionID := findNameID(regionNames, r.URL.Query().Get("region"))
		activeFilter := parseActiveFilter(r.URL.Query().Get("active"))
		minScore := parseIntOrDefault(r.URL.Query().Get("min_score"), 0)
		limit := parseIntOrDefault(r.URL.Query().Get("limit"), 10)
		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("Connection", "close")
		w.Write([]byte(searchJSON(teamID, regionID, activeFilter, minScore, limit)))
	})

	mux.HandleFunc("/batch-score", func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			w.Header().Set("Connection", "close")
			http.Error(w, "Method Not Allowed", http.StatusMethodNotAllowed)
			return
		}
		var req batchRequest
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			w.Header().Set("Content-Type", "application/json")
			w.Header().Set("Connection", "close")
			http.Error(w, `{"error":"bad json"}`, http.StatusBadRequest)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("Connection", "close")
		w.Write([]byte(batchScoreJSON(req)))
	})

	ln, err := net.Listen("tcp", ":"+port)
	if err != nil {
		fmt.Println("failed to listen:", err)
		return
	}
	fmt.Println("go catalog server on :" + port)
	srv := &http.Server{Handler: mux}
	srv.SetKeepAlivesEnabled(false)
	srv.Serve(ln)
}
