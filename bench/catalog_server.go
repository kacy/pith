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
	ID     int
	Team   string
	Region string
	Active bool
	Score  int
	Quota  int
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

func initCatalog() {
	if len(users) > 0 {
		return
	}
	teams := []string{"infra", "payments", "search", "growth", "risk", "core"}
	regions := []string{"us-east", "us-west", "eu-central", "ap-south"}
	userIndex = map[int]int{}
	for id := 1; id <= 2048; id++ {
		user := catalogUser{
			ID:     id,
			Team:   teams[(id*7)%len(teams)],
			Region: regions[(id*5)%len(regions)],
			Active: id%3 != 0,
			Score:  ((id * 37) % 900) + 100,
			Quota:  ((id * 13) % 200) + 20,
		}
		users = append(users, user)
		userIndex[id] = len(users) - 1
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

func searchJSON(team, region string, activeFilter, minScore, limit int) string {
	count := 0
	totalScore := 0
	quotaSum := 0
	ids := make([]int, 0, limit)
	for _, user := range users {
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
		if len(ids) < limit {
			ids = append(ids, user.ID)
		}
	}
	return fmt.Sprintf(`{"count":%d,"total_score":%d,"quota_sum":%d,"ids":%s}`,
		count, totalScore, quotaSum, idsJSON(ids))
}

func batchScoreJSON(req batchRequest) string {
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
	count := 0
	scoreSum := 0
	weightedTotal := 0
	ids := make([]int, 0, limit)
	for _, user := range users {
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
			user.ID, user.Team, user.Region, user.Active, user.Score, user.Quota)
	})

	mux.HandleFunc("/search", func(w http.ResponseWriter, r *http.Request) {
		team := r.URL.Query().Get("team")
		region := r.URL.Query().Get("region")
		activeFilter := parseActiveFilter(r.URL.Query().Get("active"))
		minScore := parseIntOrDefault(r.URL.Query().Get("min_score"), 0)
		limit := parseIntOrDefault(r.URL.Query().Get("limit"), 10)
		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("Connection", "close")
		w.Write([]byte(searchJSON(team, region, activeFilter, minScore, limit)))
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
