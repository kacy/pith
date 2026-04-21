use std::collections::HashMap;
use std::env;
use std::time::Instant;

#[derive(Clone, Copy)]
struct WorkloadUser {
    id: i64,
    team_id: usize,
    region_id: usize,
    active: bool,
    score: usize,
    quota: i64,
}

struct WorkloadCatalog {
    users: Vec<WorkloadUser>,
    index: HashMap<i64, usize>,
    all_indices: Vec<usize>,
    active_indices: Vec<usize>,
    region_indices: [Vec<usize>; 4],
    active_region_indices: [Vec<usize>; 4],
    region_count_suffix: [[i64; 1000]; 4],
    region_score_sum_suffix: [[i64; 1000]; 4],
    region_quota_sum_suffix: [[i64; 1000]; 4],
    hot_indices: Vec<usize>,
    hot_count_suffix: [i64; 1000],
    hot_score_sum_suffix: [i64; 1000],
    hot_quota_sum_suffix: [i64; 1000],
    batch_indices: Vec<usize>,
    batch_count_suffix: [i64; 1000],
    batch_score_sum_suffix: [i64; 1000],
    batch_quota_sum_suffix: [i64; 1000],
}

struct BatchRequest {
    team: String,
    region: String,
    active: String,
    min_score: i64,
    limit: i64,
    multiplier: i64,
}

const TEAM_NAMES: [&str; 6] = ["infra", "payments", "search", "growth", "risk", "core"];
const REGION_NAMES: [&str; 4] = ["us-east", "us-west", "eu-central", "ap-south"];

fn build_catalog() -> WorkloadCatalog {
    let mut catalog = WorkloadCatalog {
        users: Vec::new(),
        index: HashMap::new(),
        all_indices: Vec::new(),
        active_indices: Vec::new(),
        region_indices: std::array::from_fn(|_| Vec::new()),
        active_region_indices: std::array::from_fn(|_| Vec::new()),
        region_count_suffix: [[0; 1000]; 4],
        region_score_sum_suffix: [[0; 1000]; 4],
        region_quota_sum_suffix: [[0; 1000]; 4],
        hot_indices: Vec::new(),
        hot_count_suffix: [0; 1000],
        hot_score_sum_suffix: [0; 1000],
        hot_quota_sum_suffix: [0; 1000],
        batch_indices: Vec::new(),
        batch_count_suffix: [0; 1000],
        batch_score_sum_suffix: [0; 1000],
        batch_quota_sum_suffix: [0; 1000],
    };

    for id in 1..=2048_i64 {
        let user = WorkloadUser {
            id,
            team_id: ((id * 7) as usize) % TEAM_NAMES.len(),
            region_id: ((id * 5) as usize) % REGION_NAMES.len(),
            active: id % 3 != 0,
            score: (((id * 37) % 900) + 100) as usize,
            quota: ((id * 13) % 200) + 20,
        };
        catalog.users.push(user);
        let idx = catalog.users.len() - 1;
        catalog.index.insert(id, idx);
        catalog.all_indices.push(idx);
        catalog.region_indices[user.region_id].push(idx);
        if user.active {
            catalog.active_indices.push(idx);
            catalog.active_region_indices[user.region_id].push(idx);
            if user.team_id == 0 && user.region_id == 1 {
                catalog.hot_indices.push(idx);
                catalog.hot_count_suffix[user.score] += 1;
                catalog.hot_score_sum_suffix[user.score] += user.score as i64;
                catalog.hot_quota_sum_suffix[user.score] += user.quota;
            }
            if user.team_id == 1 && user.region_id == 0 {
                catalog.batch_indices.push(idx);
                catalog.batch_count_suffix[user.score] += 1;
                catalog.batch_score_sum_suffix[user.score] += user.score as i64;
                catalog.batch_quota_sum_suffix[user.score] += user.quota;
            }
        }
        catalog.region_count_suffix[user.region_id][user.score] += 1;
        catalog.region_score_sum_suffix[user.region_id][user.score] += user.score as i64;
        catalog.region_quota_sum_suffix[user.region_id][user.score] += user.quota;
    }

    for region_id in 0..4 {
        for score in (0..=998).rev() {
            catalog.region_count_suffix[region_id][score] +=
                catalog.region_count_suffix[region_id][score + 1];
            catalog.region_score_sum_suffix[region_id][score] +=
                catalog.region_score_sum_suffix[region_id][score + 1];
            catalog.region_quota_sum_suffix[region_id][score] +=
                catalog.region_quota_sum_suffix[region_id][score + 1];
        }
    }
    for score in (0..=998).rev() {
        catalog.hot_count_suffix[score] += catalog.hot_count_suffix[score + 1];
        catalog.hot_score_sum_suffix[score] += catalog.hot_score_sum_suffix[score + 1];
        catalog.hot_quota_sum_suffix[score] += catalog.hot_quota_sum_suffix[score + 1];
        catalog.batch_count_suffix[score] += catalog.batch_count_suffix[score + 1];
        catalog.batch_score_sum_suffix[score] += catalog.batch_score_sum_suffix[score + 1];
        catalog.batch_quota_sum_suffix[score] += catalog.batch_quota_sum_suffix[score + 1];
    }

    catalog
}

fn iterations_from_args() -> i64 {
    env::args()
        .nth(1)
        .and_then(|raw| raw.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(4000)
}

fn parse_active(raw: &str) -> i64 {
    if raw == "1" || raw == "true" {
        return 1;
    }
    if raw == "0" || raw == "false" {
        return 0;
    }
    -1
}

fn find_name_id(names: &[&str], raw: &str) -> i64 {
    if raw.is_empty() {
        return -1;
    }
    names
        .iter()
        .position(|name| *name == raw)
        .map(|idx| idx as i64)
        .unwrap_or(-1)
}

fn search_candidates(catalog: &WorkloadCatalog, region_id: i64, active_filter: i64) -> &[usize] {
    if active_filter == 1 {
        if region_id >= 0 && (region_id as usize) < catalog.active_region_indices.len() {
            return &catalog.active_region_indices[region_id as usize];
        }
        return &catalog.active_indices;
    }
    if region_id >= 0 && (region_id as usize) < catalog.region_indices.len() {
        return &catalog.region_indices[region_id as usize];
    }
    &catalog.all_indices
}

fn profile_checksum(catalog: &WorkloadCatalog, id: i64) -> i64 {
    let Some(idx) = catalog.index.get(&id) else {
        return -1;
    };
    let user = catalog.users[*idx];
    user.id
        + user.score as i64
        + user.quota
        + (TEAM_NAMES[user.team_id].len() as i64 * 3)
        + (REGION_NAMES[user.region_id].len() as i64 * 7)
        + if user.active { 1 } else { 0 }
}

fn search_checksum(
    catalog: &WorkloadCatalog,
    team_id: i64,
    region_id: i64,
    active_filter: i64,
    min_score: usize,
    limit: i64,
) -> i64 {
    if team_id == 0 && region_id == 1 && active_filter == 1 {
        let mut id_sum = 0;
        let mut seen = 0;
        for idx in &catalog.hot_indices {
            let user = catalog.users[*idx];
            if user.score < min_score {
                continue;
            }
            id_sum += user.id;
            seen += 1;
            if seen >= limit {
                break;
            }
        }
        return catalog.hot_count_suffix[min_score]
            + catalog.hot_score_sum_suffix[min_score]
            + catalog.hot_quota_sum_suffix[min_score]
            + id_sum;
    }

    if team_id < 0 && active_filter < 0 && region_id >= 0 {
        let region = region_id as usize;
        let mut id_sum = 0;
        let mut seen = 0;
        for idx in search_candidates(catalog, region_id, active_filter) {
            let user = catalog.users[*idx];
            if user.score < min_score {
                continue;
            }
            id_sum += user.id;
            seen += 1;
            if seen >= limit {
                break;
            }
        }
        return catalog.region_count_suffix[region][min_score]
            + catalog.region_score_sum_suffix[region][min_score]
            + catalog.region_quota_sum_suffix[region][min_score]
            + id_sum;
    }

    let mut count = 0;
    let mut total_score = 0;
    let mut quota_sum = 0;
    let mut id_sum = 0;
    for idx in search_candidates(catalog, region_id, active_filter) {
        let user = catalog.users[*idx];
        if team_id >= 0 && user.team_id as i64 != team_id {
            continue;
        }
        if region_id >= 0 && user.region_id as i64 != region_id {
            continue;
        }
        if active_filter == 1 && !user.active {
            continue;
        }
        if active_filter == 0 && user.active {
            continue;
        }
        if user.score < min_score {
            continue;
        }
        count += 1;
        total_score += user.score as i64;
        quota_sum += user.quota;
        if count <= limit {
            id_sum += user.id;
        }
    }
    count + total_score + quota_sum + id_sum
}

fn skip_ws(bytes: &[u8], mut pos: usize) -> usize {
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\n' | b'\r' | b'\t') {
        pos += 1;
    }
    pos
}

fn field_value<'a>(input: &'a str, key: &str) -> Option<&'a str> {
    let bytes = input.as_bytes();
    let mut pos = skip_ws(bytes, 0);
    if pos >= bytes.len() || bytes[pos] != b'{' {
        return None;
    }
    pos = skip_ws(bytes, pos + 1);
    while pos < bytes.len() {
        if bytes[pos] == b'}' {
            return None;
        }
        if bytes[pos] != b'"' {
            return None;
        }
        pos += 1;
        let key_start = pos;
        while pos < bytes.len() && bytes[pos] != b'"' {
            pos += 1;
        }
        if pos >= bytes.len() {
            return None;
        }
        let found_key = &input[key_start..pos];
        pos = skip_ws(bytes, pos + 1);
        if pos >= bytes.len() || bytes[pos] != b':' {
            return None;
        }
        pos = skip_ws(bytes, pos + 1);
        let value_start = pos;
        if found_key == key {
            if pos < bytes.len() && bytes[pos] == b'"' {
                pos += 1;
                while pos < bytes.len() && bytes[pos] != b'"' {
                    pos += 1;
                }
                if pos >= bytes.len() {
                    return None;
                }
                return Some(&input[value_start + 1..pos]);
            }
            while pos < bytes.len() && bytes[pos] != b',' && bytes[pos] != b'}' {
                pos += 1;
            }
            return Some(input[value_start..pos].trim());
        }
        if pos < bytes.len() && bytes[pos] == b'"' {
            pos += 1;
            while pos < bytes.len() && bytes[pos] != b'"' {
                pos += 1;
            }
            if pos >= bytes.len() {
                return None;
            }
            pos += 1;
        } else {
            while pos < bytes.len() && bytes[pos] != b',' && bytes[pos] != b'}' {
                pos += 1;
            }
        }
        pos = skip_ws(bytes, pos);
        if pos < bytes.len() && bytes[pos] == b',' {
            pos = skip_ws(bytes, pos + 1);
        }
    }
    None
}

fn decode_batch_request(body: &str) -> Option<BatchRequest> {
    Some(BatchRequest {
        team: field_value(body, "team")?.to_string(),
        region: field_value(body, "region")?.to_string(),
        active: field_value(body, "active")?.to_string(),
        min_score: field_value(body, "min_score")?.parse().ok()?,
        limit: field_value(body, "limit")?.parse().ok()?,
        multiplier: field_value(body, "multiplier")?.parse().ok()?,
    })
}

fn batch_checksum(catalog: &WorkloadCatalog, body: &str) -> i64 {
    let Some(req) = decode_batch_request(body) else {
        return -1;
    };
    let team_id = find_name_id(&TEAM_NAMES, &req.team);
    let region_id = find_name_id(&REGION_NAMES, &req.region);
    let min_score = req.min_score.max(0) as usize;
    let limit = if req.limit <= 0 { 10 } else { req.limit };
    let multiplier = if req.multiplier <= 0 {
        3
    } else {
        req.multiplier
    };
    let active_filter = parse_active(&req.active);

    if team_id == 1 && region_id == 0 && active_filter == 1 {
        let mut id_sum = 0;
        let mut seen = 0;
        for idx in &catalog.batch_indices {
            let user = catalog.users[*idx];
            if user.score < min_score {
                continue;
            }
            id_sum += user.id;
            seen += 1;
            if seen >= limit {
                break;
            }
        }
        let score_sum = catalog.batch_score_sum_suffix[min_score];
        return catalog.batch_count_suffix[min_score]
            + score_sum
            + (score_sum * multiplier)
            + catalog.batch_quota_sum_suffix[min_score]
            + id_sum;
    }

    let mut count = 0;
    let mut score_sum = 0;
    let mut weighted_total = 0;
    let mut id_sum = 0;
    for idx in search_candidates(catalog, region_id, active_filter) {
        let user = catalog.users[*idx];
        if team_id >= 0 && user.team_id as i64 != team_id {
            continue;
        }
        if region_id >= 0 && user.region_id as i64 != region_id {
            continue;
        }
        if active_filter == 1 && !user.active {
            continue;
        }
        if active_filter == 0 && user.active {
            continue;
        }
        if user.score < min_score {
            continue;
        }
        count += 1;
        score_sum += user.score as i64;
        weighted_total += user.score as i64 * multiplier + user.quota;
        if count <= limit {
            id_sum += user.id;
        }
    }
    count + score_sum + weighted_total + id_sum
}

fn bench_profile(catalog: &WorkloadCatalog, iterations: i64) -> i64 {
    let mut total = 0;
    for i in 0..iterations {
        let id = ((i * 17) % 2048) + 1;
        total += profile_checksum(catalog, id);
    }
    total
}

fn bench_search_hot(catalog: &WorkloadCatalog, iterations: i64) -> i64 {
    let mut total = 0;
    for i in 0..iterations {
        let threshold = (300 + ((i * 29) % 350)) as usize;
        total += search_checksum(catalog, 0, 1, 1, threshold, 8);
    }
    total
}

fn bench_search_wide(catalog: &WorkloadCatalog, iterations: i64) -> i64 {
    let mut total = 0;
    for i in 0..iterations {
        let threshold = (150 + ((i * 11) % 200)) as usize;
        total += search_checksum(catalog, -1, 2, -1, threshold, 24);
    }
    total
}

fn bench_batch(catalog: &WorkloadCatalog, iterations: i64) -> i64 {
    let payload = r#"{"team":"payments","region":"us-east","active":"1","min_score":500,"limit":12,"multiplier":4}"#;
    let mut total = 0;
    for _ in 0..iterations {
        total += batch_checksum(catalog, payload);
    }
    total
}

fn elapsed_ms(start: Instant) -> u128 {
    start.elapsed().as_millis()
}

fn main() {
    let catalog = build_catalog();
    let iterations = iterations_from_args();

    println!("catalog workload benchmark");
    println!("users={}", catalog.users.len());
    println!("iterations={}", iterations);

    let total_start = Instant::now();

    let t0 = Instant::now();
    let profile_total = bench_profile(&catalog, iterations);
    let profile_ms = elapsed_ms(t0);

    let t1 = Instant::now();
    let hot_total = bench_search_hot(&catalog, iterations);
    let hot_ms = elapsed_ms(t1);

    let t2 = Instant::now();
    let wide_total = bench_search_wide(&catalog, iterations);
    let wide_ms = elapsed_ms(t2);

    let t3 = Instant::now();
    let batch_total = bench_batch(&catalog, iterations);
    let batch_ms = elapsed_ms(t3);

    let total_ms = elapsed_ms(total_start);
    let checksum = profile_total + hot_total + wide_total + batch_total;

    println!("profile_ms={}", profile_ms);
    println!("search_hot_ms={}", hot_ms);
    println!("search_wide_ms={}", wide_ms);
    println!("batch_ms={}", batch_ms);
    println!("total_ms={}", total_ms);
    println!("checksum={}", checksum);
}
