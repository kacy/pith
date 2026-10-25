// json_decode_shapes: rust counterpart of bench/json_decode_shapes.pith.
//
// typed json decoding, one struct shape per run, with serde_json and
// serde's derive: each round calls serde_json::from_slice::<T> on the same
// document bytes, the way std.json's decode[T] is called in the pith
// version. `list_walk` parses into a serde_json::Value first and decodes
// each element of `items` from it with Small::deserialize, the analogue of
// json.parse plus decode_object. the documents, shapes and checksum are
// the same as the pith program's, so equal output means equal decoding.
//
// build: cargo build --release --manifest-path bench/json_decode_shapes_rust/Cargo.toml
// run:   ./bench/json_decode_shapes_rust/target/release/json_decode_shapes_rust <shape> <size> <rounds>

use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::HashMap;
use std::env;

#[derive(Deserialize)]
struct Small {
    id: i64,
    name: String,
    active: bool,
}

#[derive(Deserialize)]
struct Wide {
    f01: i64,
    f02: String,
    f03: bool,
    f04: i64,
    f05: String,
    f06: i64,
    f07: i64,
    f08: String,
    f09: bool,
    f10: i64,
    f11: String,
    f12: i64,
    f13: i64,
    f14: String,
    f15: bool,
    f16: i64,
    f17: String,
    f18: i64,
    f19: i64,
    f20: String,
}

#[derive(Deserialize)]
struct Inner {
    city: String,
    zip: i64,
}

#[derive(Deserialize)]
struct Nested {
    name: String,
    addr: Inner,
    score: i64,
}

#[derive(Deserialize)]
struct Sample {
    id: i64,
    name: String,
    value: f64,
    delta: f64,
}

#[derive(Deserialize)]
struct SampleInts {
    id: i64,
    name: String,
    value: i64,
    delta: i64,
}

#[derive(Deserialize)]
struct Items {
    items: Vec<Small>,
}

#[derive(Deserialize)]
struct Ids {
    name: String,
    ids: Vec<i64>,
}

#[derive(Deserialize)]
struct Grid {
    name: String,
    cells: Vec<Vec<i64>>,
}

#[derive(Deserialize)]
struct Scores {
    name: String,
    scores: HashMap<String, i64>,
}

#[derive(Deserialize)]
struct Catalog {
    name: String,
    items: HashMap<String, Small>,
}

fn text_of(width: usize) -> String {
    (0..width).map(|i| (b'a' + (i % 26) as u8) as char).collect()
}

fn small_doc(width: usize) -> String {
    format!(r#"{{"id":7,"name":"{}","active":true}}"#, text_of(width))
}

fn wide_doc(width: usize) -> String {
    let text = format!("\"{}\"", text_of(width));
    let fields: Vec<String> = (1..=20)
        .map(|i| {
            let value = if i % 3 == 2 {
                text.clone()
            } else if i % 6 == 3 {
                "true".to_string()
            } else {
                (i * 11).to_string()
            };
            format!("\"f{}{}\":{}", i / 10, i % 10, value)
        })
        .collect();
    format!("{{{}}}", fields.join(","))
}

fn nested_doc(width: usize) -> String {
    let t = text_of(width);
    format!(r#"{{"name":"{t}","addr":{{"city":"{t}","zip":75001}},"score":42}}"#)
}

fn sample_doc(width: usize, floats: bool) -> String {
    let (value, delta) = if floats { ("12.5", "-0.25") } else { ("1250", "-2500") };
    format!(r#"{{"id":7,"name":"{}","value":{value},"delta":{delta}}}"#, text_of(width))
}

fn list_doc(count: usize) -> String {
    let items: Vec<String> = (0..count)
        .map(|i| format!(r#"{{"id":{i},"name":"item{i}","active":false}}"#))
        .collect();
    format!(r#"{{"items":[{}]}}"#, items.join(","))
}

fn list_scalar_doc(count: usize) -> String {
    let ids: Vec<String> = (0..count).map(|i| (i * 7).to_string()).collect();
    format!(r#"{{"name":"ids","ids":[{}]}}"#, ids.join(","))
}

fn nested_list_doc(count: usize) -> String {
    let cells: Vec<String> = (0..count)
        .map(|i| format!("[{},{},{},{}]", i, i * 2, i * 3, i * 4))
        .collect();
    format!(r#"{{"name":"grid","cells":[{}]}}"#, cells.join(","))
}

fn map_doc(count: usize) -> String {
    let members: Vec<String> = (0..count).map(|i| format!(r#""k{i}":{}"#, i * 7)).collect();
    format!(r#"{{"name":"scores","scores":{{{}}}}}"#, members.join(","))
}

fn map_struct_doc(count: usize) -> String {
    let members: Vec<String> = (0..count)
        .map(|i| format!(r#""k{i}":{{"id":{i},"name":"item{i}","active":false}}"#))
        .collect();
    format!(r#"{{"name":"catalog","items":{{{}}}}}"#, members.join(","))
}

fn small_sum(r: Small) -> i64 {
    r.id + r.name.len() as i64 + r.active as i64
}

fn wide_sum(r: Wide) -> i64 {
    let ints = r.f01 + r.f04 + r.f06 + r.f07 + r.f10 + r.f12 + r.f13 + r.f16 + r.f18 + r.f19;
    let lens = [&r.f02, &r.f05, &r.f08, &r.f11, &r.f14, &r.f17, &r.f20]
        .iter()
        .map(|s| s.len() as i64)
        .sum::<i64>();
    ints + lens + r.f03 as i64 + r.f09 as i64 + r.f15 as i64
}

fn nested_sum(r: Nested) -> i64 {
    r.name.len() as i64 + r.addr.city.len() as i64 + r.addr.zip + r.score
}

fn float_sum(r: Sample) -> i64 {
    r.id + r.name.len() as i64 + (r.value * 10.0) as i64 + (r.delta * 100.0) as i64
}

fn ints_sum(r: SampleInts) -> i64 {
    r.id + r.name.len() as i64 + r.value + r.delta
}

fn list_sum(r: Items) -> i64 {
    r.items.iter().map(|it| it.id + it.name.len() as i64).sum()
}

fn list_scalar_sum(r: Ids) -> i64 {
    r.name.len() as i64 + r.ids.iter().sum::<i64>()
}

fn nested_list_sum(r: Grid) -> i64 {
    r.name.len() as i64 + r.cells.iter().flatten().sum::<i64>()
}

fn map_sum(r: Scores) -> i64 {
    r.name.len() as i64 + r.scores.len() as i64 + r.scores.values().sum::<i64>()
}

fn map_struct_sum(r: Catalog) -> i64 {
    r.name.len() as i64
        + r.items.len() as i64
        + r.items.values().map(|it| it.id + it.name.len() as i64).sum::<i64>()
}

fn list_walk_sum(doc: &str) -> i64 {
    let root: serde_json::Value = serde_json::from_str(doc).unwrap_or_default();
    let mut v = 0;
    if let Some(arr) = root.get("items").and_then(|a| a.as_array()) {
        for el in arr {
            v += match Small::deserialize(el) {
                Ok(it) => it.id + it.name.len() as i64,
                Err(_) => -1,
            };
        }
    }
    v
}

// decode `doc` into T `rounds` times and fold each result through `sum`;
// a failed decode contributes `fallback`, as the pith `catch` arms do.
fn run<T: DeserializeOwned>(doc: &str, rounds: usize, fallback: i64, sum: fn(T) -> i64) -> i64 {
    let bytes = doc.as_bytes();
    (0..rounds)
        .map(|_| serde_json::from_slice::<T>(bytes).map(sum).unwrap_or(fallback))
        .sum()
}

fn main() {
    let argv: Vec<String> = env::args().collect();
    let shape = argv.get(1).map(String::as_str).unwrap_or("small");
    let int_arg = |at: usize, fallback: usize| {
        argv.get(at).and_then(|s| s.parse::<usize>().ok()).filter(|&v| v > 0).unwrap_or(fallback)
    };
    let size = int_arg(2, 8);
    let rounds = int_arg(3, 20000);
    let total = match shape {
        "small" => run(&small_doc(size), rounds, -1, small_sum),
        "wide" => run(&wide_doc(size), rounds, -1, wide_sum),
        "nested" => run(&nested_doc(size), rounds, -1, nested_sum),
        "list" => run(&list_doc(size), rounds, 0, list_sum),
        "list_walk" => {
            let doc = list_doc(size);
            (0..rounds).map(|_| list_walk_sum(&doc)).sum()
        }
        "list_scalar" => run(&list_scalar_doc(size), rounds, 0, list_scalar_sum),
        "nested_list" => run(&nested_list_doc(size), rounds, 0, nested_list_sum),
        "map" => run(&map_doc(size), rounds, 0, map_sum),
        "map_struct" => run(&map_struct_doc(size), rounds, 0, map_struct_sum),
        "float" => run(&sample_doc(size, true), rounds, -1, float_sum),
        "ints" => run(&sample_doc(size, false), rounds, -1, ints_sum),
        _ => {
            println!("unknown shape: {shape}");
            std::process::exit(1);
        }
    };
    println!("shape={shape} size={size} rounds={rounds}");
    println!("checksum={total}");
}
