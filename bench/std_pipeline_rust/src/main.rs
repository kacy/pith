use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use url::Url;

#[derive(Clone, Copy)]
struct PipelineConfig {
    min_score: i64,
    multiplier: i64,
    include_inactive: bool,
}

#[derive(Default)]
struct PipelineStats {
    active_count: i64,
    selected: i64,
    score_sum: i64,
    quota_sum: i64,
    url_path_sum: i64,
    path_part_sum: i64,
    note_hash_sum: i64,
}

#[derive(Serialize)]
struct ReportPayload {
    records: i64,
    active: i64,
    selected: i64,
    score_sum: i64,
    quota_sum: i64,
    url_path_sum: i64,
    path_part_sum: i64,
    note_hash_sum: i64,
}

fn records_from_args() -> i64 {
    env::args()
        .nth(1)
        .and_then(|arg| arg.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(50000)
}

fn elapsed_ms(start: Instant) -> i64 {
    start.elapsed().as_millis() as i64
}

fn region_for(i: i64) -> &'static str {
    match i % 4 {
        0 => "north",
        1 => "south",
        2 => "east",
        _ => "west",
    }
}

fn active_for(i: i64) -> bool {
    i % 3 != 0
}

fn build_config() -> PipelineConfig {
    let toml_text = "[limits]\nmin_score = 37\n[output]\nname = \"report\"";
    let json_text = r#"{"limits":{"multiplier":3},"output":{"gzip":true}}"#;
    let base: toml::Value = toml::from_str(toml_text).expect("parse toml config");
    let overrides: serde_json::Value = serde_json::from_str(json_text).expect("parse json config");
    PipelineConfig {
        min_score: base["limits"]["min_score"].as_integer().unwrap_or(0),
        multiplier: overrides["limits"]["multiplier"].as_i64().unwrap_or(1),
        include_inactive: overrides["output"]["gzip"].as_bool().unwrap_or(true),
    }
}

fn make_rows(records: i64) -> Vec<Vec<String>> {
    let mut rows = Vec::with_capacity(records as usize + 1);
    rows.push(
        [
            "id", "name", "region", "active", "score", "quota", "url", "joined", "note", "path",
        ]
        .iter()
        .map(|part| part.to_string())
        .collect(),
    );
    for i in 0..records {
        let region = region_for(i);
        let score = (i * 17 + 11) % 100;
        let quota = (i * 13 + 7) % 50;
        let active = active_for(i);
        let note = format!("user {i}, region {region}");
        let url_text = format!("https://example.com/api/{region}/users/{i}?score={score}");
        let joined = format!("2026-{}-{}", (i % 12) + 1, (i % 28) + 1);
        let user_path = format!("data//{region}/./users/../users/{i}.json");
        rows.push(vec![
            i.to_string(),
            format!("user-{i}"),
            region.to_string(),
            active.to_string(),
            score.to_string(),
            quota.to_string(),
            url_text,
            joined,
            note,
            user_path,
        ]);
    }
    rows
}

fn write_csv(path: &Path, rows: &[Vec<String>]) -> Result<(), Box<dyn std::error::Error>> {
    let mut writer = csv::Writer::from_path(path)?;
    for row in rows {
        writer.write_record(row)?;
    }
    writer.flush()?;
    Ok(())
}

fn read_csv(path: &Path) -> Result<Vec<Vec<String>>, Box<dyn std::error::Error>> {
    let mut reader = csv::Reader::from_path(path)?;
    let mut rows = Vec::new();
    for result in reader.records() {
        let record = result?;
        rows.push(record.iter().map(|field| field.to_string()).collect());
    }
    Ok(rows)
}

fn fnv1a_string(text: &str) -> i64 {
    let mut h: u32 = 2_166_136_261;
    for byte in text.as_bytes() {
        h ^= *byte as u32;
        h = h.wrapping_mul(16_777_619);
    }
    h as i64
}

fn clean_path_parts(text: &str) -> i64 {
    let mut stack: Vec<&str> = Vec::new();
    for part in text.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            stack.pop();
            continue;
        }
        stack.push(part);
    }
    stack.len() as i64
}

fn transform(rows: &[Vec<String>], cfg: PipelineConfig) -> PipelineStats {
    let mut stats = PipelineStats::default();
    for row in rows {
        let score = row[4].parse::<i64>().unwrap_or(0);
        let quota = row[5].parse::<i64>().unwrap_or(0);
        let active = row[3] == "true";
        let parsed_url = Url::parse(&row[6]).expect("parse url");
        stats.url_path_sum += parsed_url.path().len() as i64;
        stats.path_part_sum += clean_path_parts(&row[9]);
        stats.note_hash_sum += fnv1a_string(&row[8]);
        if active {
            stats.active_count += 1;
        }
        if (active || cfg.include_inactive) && score >= cfg.min_score {
            stats.selected += 1;
            stats.score_sum += score * cfg.multiplier;
            stats.quota_sum += quota;
        }
    }
    stats
}

fn write_report(stats: &PipelineStats, records: i64) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&ReportPayload {
        records,
        active: stats.active_count,
        selected: stats.selected,
        score_sum: stats.score_sum,
        quota_sum: stats.quota_sum,
        url_path_sum: stats.url_path_sum,
        path_part_sum: stats.path_part_sum,
        note_hash_sum: stats.note_hash_sum,
    })
}

fn gzip_round_trip(data: &[u8]) -> Result<(Vec<u8>, Vec<u8>), Box<dyn std::error::Error>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    let compressed = encoder.finish()?;
    let mut decoder = GzDecoder::new(compressed.as_slice());
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed)?;
    Ok((compressed, decompressed))
}

fn digest_score(digest: &str) -> i64 {
    digest.as_bytes().iter().map(|byte| *byte as i64).sum()
}

fn walk_score(root: &Path) -> Result<i64, Box<dyn std::error::Error>> {
    fn visit(path: &Path, total: &mut i64) -> Result<(), Box<dyn std::error::Error>> {
        let metadata = fs::metadata(path)?;
        let name_len = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .len() as i64;
        *total += name_len + metadata.len() as i64;
        if metadata.is_dir() {
            *total += 17;
            for entry in fs::read_dir(path)? {
                visit(&entry?.path(), total)?;
            }
        }
        Ok(())
    }
    let mut total = 0;
    visit(root, &mut total)?;
    Ok(total)
}

fn print_metric(name: &str, value: i64) {
    println!("{name}={value}");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let records = records_from_args();
    let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let work_dir: PathBuf = env::temp_dir().join(format!("pith-std-pipeline-{unique}"));
    fs::create_dir_all(&work_dir)?;
    let csv_path = work_dir.join("input.csv");
    let json_path = work_dir.join("report.json");
    let gzip_path = work_dir.join("report.json.gz");
    let total_start = Instant::now();

    let start = Instant::now();
    let cfg = build_config();
    let config_ms = elapsed_ms(start);

    let start = Instant::now();
    let rows = make_rows(records);
    write_csv(&csv_path, &rows)?;
    let csv_write_ms = elapsed_ms(start);

    let start = Instant::now();
    let parsed_rows = read_csv(&csv_path)?;
    let csv_read_ms = elapsed_ms(start);

    let start = Instant::now();
    let stats = transform(&parsed_rows, cfg);
    let transform_ms = elapsed_ms(start);

    let start = Instant::now();
    let report = write_report(&stats, records)?;
    fs::write(&json_path, &report)?;
    let json_ms = elapsed_ms(start);

    let start = Instant::now();
    let (compressed, decompressed) = gzip_round_trip(&report)?;
    fs::write(&gzip_path, compressed)?;
    let digest = format!("{:x}", Sha256::digest(&decompressed));
    let gzip_hash_ms = elapsed_ms(start);

    let start = Instant::now();
    let fs_score = walk_score(&work_dir)?;
    let fs_ms = elapsed_ms(start);

    let checksum = stats.score_sum
        + stats.quota_sum
        + stats.url_path_sum
        + stats.path_part_sum
        + stats.note_hash_sum
        + digest_score(&digest)
        + decompressed.len() as i64
        + fs_score * 0;
    let total_ms = elapsed_ms(total_start);

    fs::remove_dir_all(&work_dir)?;

    println!("std pipeline benchmark");
    print_metric("records", records);
    print_metric("config_ms", config_ms);
    print_metric("csv_write_ms", csv_write_ms);
    print_metric("csv_read_ms", csv_read_ms);
    print_metric("transform_ms", transform_ms);
    print_metric("json_ms", json_ms);
    print_metric("gzip_hash_ms", gzip_hash_ms);
    print_metric("fs_ms", fs_ms);
    print_metric("total_ms", total_ms);
    print_metric("checksum", checksum);
    Ok(())
}
