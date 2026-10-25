// rust counterpart to bench/catalog_server.go and bench/catalog_server.pith:
// the same synthetic catalog, the same endpoints and the same json, served
// by axum on a tokio multi-thread runtime. like the go server it answers
// every request with "Connection: close", so each request costs a fresh
// connection, which is what bench/catalog_bench.go measures.
//
//   GET  /health            readiness check
//   GET  /profile?id=N      single-record lookup
//   GET  /search?...        filtered scans and aggregate summaries
//   POST /batch-score       json body parsing plus aggregate scoring
//
// worker threads come from TOKIO_WORKERS (the tokio default, one per cpu,
// when it is unset).
//
// build with: (cd bench/catalog_server_rust && cargo build --release)
// run with:   bench/catalog_server_rust/target/release/catalog_server_rust 9103
// and point the benchmark at it: go run bench/catalog_bench.go 9101 9103
use std::collections::HashMap;
use std::env;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::header::{CONNECTION, CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::serve::ListenerExt;
use axum::Router;
use serde::Deserialize;

const TEAM_NAMES: [&str; 6] = ["infra", "payments", "search", "growth", "risk", "core"];
const REGION_NAMES: [&str; 4] = ["us-east", "us-west", "eu-central", "ap-south"];

#[derive(Clone, Copy)]
struct User {
    id: i64,
    team_id: i64,
    region_id: i64,
    active: bool,
    score: i64,
    quota: i64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct BatchRequest {
    team: String,
    region: String,
    active: String,
    min_score: i64,
    limit: i64,
    multiplier: i64,
}

// running totals of the users with a score at or above each value, built
// as a histogram and then summed from the top down
struct Suffix {
    count: [i64; 1000],
    score_sum: [i64; 1000],
    quota_sum: [i64; 1000],
}

impl Suffix {
    fn new() -> Self {
        Suffix {
            count: [0; 1000],
            score_sum: [0; 1000],
            quota_sum: [0; 1000],
        }
    }

    fn add(&mut self, user: &User) {
        let s = user.score as usize;
        self.count[s] += 1;
        self.score_sum[s] += user.score;
        self.quota_sum[s] += user.quota;
    }

    fn accumulate(&mut self) {
        for s in (0..=998).rev() {
            self.count[s] += self.count[s + 1];
            self.score_sum[s] += self.score_sum[s + 1];
            self.quota_sum[s] += self.quota_sum[s + 1];
        }
    }
}

struct Catalog {
    users: Vec<User>,
    user_index: HashMap<i64, usize>,
    all: Vec<usize>,
    active: Vec<usize>,
    region: [Vec<usize>; 4],
    active_region: [Vec<usize>; 4],
    region2: Suffix,
    hot_indices: Vec<usize>,
    hot: Suffix,
    batch_indices: Vec<usize>,
    batch: Suffix,
}

impl Catalog {
    fn new() -> Self {
        let mut c = Catalog {
            users: Vec::new(),
            user_index: HashMap::new(),
            all: Vec::new(),
            active: Vec::new(),
            region: Default::default(),
            active_region: Default::default(),
            region2: Suffix::new(),
            hot_indices: Vec::new(),
            hot: Suffix::new(),
            batch_indices: Vec::new(),
            batch: Suffix::new(),
        };
        for id in 1..=2048i64 {
            let user = User {
                id,
                team_id: (id * 7) % TEAM_NAMES.len() as i64,
                region_id: (id * 5) % REGION_NAMES.len() as i64,
                active: id % 3 != 0,
                score: ((id * 37) % 900) + 100,
                quota: ((id * 13) % 200) + 20,
            };
            c.users.push(user);
            let idx = c.users.len() - 1;
            c.user_index.insert(id, idx);
            c.all.push(idx);
            c.region[user.region_id as usize].push(idx);
            if user.active {
                c.active.push(idx);
                c.active_region[user.region_id as usize].push(idx);
                if user.team_id == 0 && user.region_id == 1 {
                    c.hot_indices.push(idx);
                    c.hot.add(&user);
                }
                if user.team_id == 1 && user.region_id == 0 {
                    c.batch_indices.push(idx);
                    c.batch.add(&user);
                }
            }
            if user.region_id == 2 {
                c.region2.add(&user);
            }
        }
        c.region2.accumulate();
        c.hot.accumulate();
        c.batch.accumulate();
        c
    }

    fn search_candidates(&self, region_id: i64, active_filter: i64) -> &[usize] {
        let region = usize::try_from(region_id).ok().filter(|&r| r < 4);
        match (active_filter == 1, region) {
            (true, Some(r)) => &self.active_region[r],
            (true, None) => &self.active,
            (false, Some(r)) => &self.region[r],
            (false, None) => &self.all,
        }
    }

    // the first `limit` ids of `indices` with a score of at least min_score
    fn first_ids(&self, indices: &[usize], min_score: i64, limit: i64) -> Vec<i64> {
        let mut ids = Vec::new();
        for &idx in indices {
            let user = &self.users[idx];
            if user.score < min_score {
                continue;
            }
            if (ids.len() as i64) < limit {
                ids.push(user.id);
            } else {
                break;
            }
        }
        ids
    }

    // the users of the generic scan that match every filter
    fn matches(
        &self,
        team_id: i64,
        region_id: i64,
        active_filter: i64,
        min_score: i64,
    ) -> impl Iterator<Item = &User> {
        self.search_candidates(region_id, active_filter)
            .iter()
            .map(|&idx| &self.users[idx])
            .filter(move |u| {
                (team_id < 0 || u.team_id == team_id)
                    && (region_id < 0 || u.region_id == region_id)
                    && !(active_filter == 1 && !u.active)
                    && !(active_filter == 0 && u.active)
                    && u.score >= min_score
            })
    }

    fn search_json(
        &self,
        team_id: i64,
        region_id: i64,
        active_filter: i64,
        min_score: i64,
        limit: i64,
    ) -> String {
        let fast = if team_id == 0 && region_id == 1 && active_filter == 1 {
            Some((&self.hot, &self.hot_indices[..]))
        } else if team_id < 0 && region_id == 2 && active_filter < 0 {
            Some((&self.region2, &self.region[2][..]))
        } else {
            None
        };
        let (count, total_score, quota_sum, ids) = match fast {
            Some((suffix, indices)) => {
                let m = min_score as usize;
                (
                    suffix.count[m],
                    suffix.score_sum[m],
                    suffix.quota_sum[m],
                    self.first_ids(indices, min_score, limit),
                )
            }
            None => {
                let (mut count, mut total_score, mut quota_sum, mut ids) = (0, 0, 0, Vec::new());
                for user in self.matches(team_id, region_id, active_filter, min_score) {
                    count += 1;
                    total_score += user.score;
                    quota_sum += user.quota;
                    if (ids.len() as i64) < limit {
                        ids.push(user.id);
                    }
                }
                (count, total_score, quota_sum, ids)
            }
        };
        format!(
            r#"{{"count":{count},"total_score":{total_score},"quota_sum":{quota_sum},"ids":{}}}"#,
            ids_json(&ids)
        )
    }

    fn batch_score_json(&self, req: &BatchRequest) -> String {
        let team_id = find_name_id(&TEAM_NAMES, &req.team);
        let region_id = find_name_id(&REGION_NAMES, &req.region);
        let min_score = req.min_score.max(0);
        let limit = if req.limit <= 0 { 10 } else { req.limit };
        let multiplier = if req.multiplier <= 0 {
            3
        } else {
            req.multiplier
        };
        let active_filter = parse_active_filter(&req.active);
        let (count, score_sum, weighted_total, ids) =
            if team_id == 1 && region_id == 0 && active_filter == 1 {
                let m = min_score as usize;
                let b = &self.batch;
                let weighted_total = b.score_sum[m] * multiplier + b.quota_sum[m];
                (
                    b.count[m],
                    b.score_sum[m],
                    weighted_total,
                    self.first_ids(&self.batch_indices, min_score, limit),
                )
            } else {
                let (mut count, mut score_sum, mut weighted_total, mut ids) = (0, 0, 0, Vec::new());
                for user in self.matches(team_id, region_id, active_filter, min_score) {
                    count += 1;
                    score_sum += user.score;
                    weighted_total += user.score * multiplier + user.quota;
                    if (ids.len() as i64) < limit {
                        ids.push(user.id);
                    }
                }
                (count, score_sum, weighted_total, ids)
            };
        format!(
            r#"{{"count":{count},"score_sum":{score_sum},"weighted_total":{weighted_total},"ids":{}}}"#,
            ids_json(&ids)
        )
    }
}

fn ids_json(ids: &[i64]) -> String {
    let parts: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
    format!("[{}]", parts.join(","))
}

fn parse_int_or_default(raw: &str, fallback: i64) -> i64 {
    match raw.parse::<i64>() {
        Ok(v) if v > 0 => v,
        _ => fallback,
    }
}

fn parse_active_filter(raw: &str) -> i64 {
    match raw {
        "1" | "true" => 1,
        "0" | "false" => 0,
        _ => -1,
    }
}

fn find_name_id(names: &[&str], raw: &str) -> i64 {
    names
        .iter()
        .position(|n| *n == raw)
        .map_or(-1, |i| i as i64)
}

// the first value of `name` in the query, or "", as url.Values.Get returns
fn param<'a>(params: &'a [(String, String)], name: &str) -> &'a str {
    params
        .iter()
        .find(|(k, _)| k == name)
        .map_or("", |(_, v)| v.as_str())
}

fn json(body: String) -> Response {
    ([(CONTENT_TYPE, "application/json")], body).into_response()
}

// net/http's http.Error, which always answers in text/plain
fn http_error(status: StatusCode, msg: &str) -> Response {
    let headers = [
        (CONTENT_TYPE, "text/plain; charset=utf-8"),
        (X_CONTENT_TYPE_OPTIONS, "nosniff"),
    ];
    (status, headers, format!("{msg}\n")).into_response()
}

fn not_found() -> Response {
    http_error(StatusCode::NOT_FOUND, "404 page not found")
}

type Shared = State<Arc<Catalog>>;
type Params = Query<Vec<(String, String)>>;

async fn health() -> Response {
    ([(CONTENT_TYPE, "text/plain")], "ok").into_response()
}

async fn profile(State(c): Shared, Query(q): Params) -> Response {
    let id = parse_int_or_default(param(&q, "id"), 1);
    let Some(&idx) = c.user_index.get(&id) else {
        return not_found();
    };
    let u = &c.users[idx];
    json(format!(
        r#"{{"id":{},"team":"{}","region":"{}","active":{},"score":{},"quota":{}}}"#,
        u.id,
        TEAM_NAMES[u.team_id as usize],
        REGION_NAMES[u.region_id as usize],
        u.active,
        u.score,
        u.quota
    ))
}

async fn search(State(c): Shared, Query(q): Params) -> Response {
    let team_id = find_name_id(&TEAM_NAMES, param(&q, "team"));
    let region_id = find_name_id(&REGION_NAMES, param(&q, "region"));
    let active_filter = parse_active_filter(param(&q, "active"));
    let min_score = parse_int_or_default(param(&q, "min_score"), 0);
    let limit = parse_int_or_default(param(&q, "limit"), 10);
    json(c.search_json(team_id, region_id, active_filter, min_score, limit))
}

async fn batch_score(State(c): Shared, method: Method, body: Bytes) -> Response {
    if method != Method::POST {
        return http_error(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed");
    }
    // json.Decoder.Decode reads the first value and ignores what follows
    match serde_json::Deserializer::from_slice(&body)
        .into_iter::<BatchRequest>()
        .next()
    {
        Some(Ok(req)) => json(c.batch_score_json(&req)),
        _ => http_error(StatusCode::BAD_REQUEST, r#"{"error":"bad json"}"#),
    }
}

// the go server turns keep-alives off, so every response it sends carries
// "Connection: close"; hyper closes the connection after such a response
async fn close_connection(mut resp: Response) -> Response {
    resp.headers_mut()
        .insert(CONNECTION, HeaderValue::from_static("close"));
    resp
}

fn main() {
    let port = env::args()
        .nth(1)
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "9103".to_string());
    let catalog = Arc::new(Catalog::new());
    let app = Router::new()
        .route("/health", any(health))
        .route("/profile", any(profile))
        .route("/search", any(search))
        .route("/batch-score", any(batch_score))
        .fallback(|| async { not_found() })
        .layer(axum::middleware::map_response(close_connection))
        .with_state(catalog);

    let mut builder = tokio::runtime::Builder::new_multi_thread();
    if let Some(n) = env::var("TOKIO_WORKERS").ok().and_then(|v| v.parse().ok()) {
        builder.worker_threads(n);
    }
    let rt = builder.enable_all().build().expect("tokio runtime");
    rt.block_on(async {
        let ln = match tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await {
            Ok(ln) => ln,
            Err(e) => {
                println!("failed to listen: {e}");
                return;
            }
        };
        println!("rust catalog server on :{port}");
        // go's net package turns nagle off on every tcp conn; match it
        let ln = ln.tap_io(|tcp| {
            let _ = tcp.set_nodelay(true);
        });
        axum::serve(ln, app).await.expect("serve");
    });
}
