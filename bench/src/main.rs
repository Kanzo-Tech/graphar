//! The measurements of "Leveraging Parquet for representing RDF data", written to one results
//! file that the paper's `numbers.tex` copies from and from nowhere else:
//!
//! ```sh
//! cargo run --release -p graphar-bench -- [--scales 1,10,60] [--runs 5] [--work DIR] [--out bench/results.json]
//! ```
//!
//! For each scale of the synthetic dataset ([`generate`]) it writes the same triples five ways —
//! N-Triples, N-Triples gzipped, HDT, an S/P/O Parquet table (SPO) and GraphAr through
//! `graphar-rdf` (GAR) — and records each one's size, the time to write it, the share of GAR's
//! write spent counting (`graphar-stats`), and the time of six accesses: the triples of one
//! subject, of one predicate, of one predicate and object, a date range, and the out- and
//! in-neighbourhood of a vertex. GAR and SPO are read by DuckDB, HDT by the `hdt` crate, and
//! N-Triples, which has no access path, is timed by a full parse. Every layout's answer to every
//! access is counted, and the counts must agree, or the run fails.
//!
//! Every time is the median of `--runs` runs after one unmeasured run. Files are local; remote
//! access over object storage is not measured here.

mod generate;
mod layouts;
mod queries;

use std::path::PathBuf;
use std::time::Instant;

use serde_json::{Value, json};

struct Args {
    scales: Vec<u32>,
    runs: usize,
    work: PathBuf,
    out: PathBuf,
}

fn args() -> Args {
    let mut a = Args {
        scales: vec![1, 10, 60],
        runs: 5,
        work: std::env::temp_dir().join("graphar-bench"),
        out: PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/results.json")),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--scales" => {
                a.scales = value
                    .split(',')
                    .map(|s| s.parse().expect("a scale"))
                    .collect()
            }
            "--runs" => a.runs = value.parse().expect("a number of runs"),
            "--work" => a.work = PathBuf::from(value),
            "--out" => a.out = PathBuf::from(value),
            other => panic!("unknown flag {other}"),
        }
    }
    a
}

/// The median of `runs` timings of `f`, in seconds, after one unmeasured run; and `f`'s last
/// result.
pub fn timed<T>(runs: usize, mut f: impl FnMut() -> T) -> (f64, T) {
    let mut last = f();
    let mut times = Vec::with_capacity(runs);
    for _ in 0..runs.max(1) {
        let start = Instant::now();
        last = f();
        times.push(start.elapsed().as_secs_f64());
    }
    times.sort_by(f64::total_cmp);
    (times[times.len() / 2], last)
}

fn command(cmd: &str, args: &[&str]) -> String {
    std::process::Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default()
}

fn host() -> Value {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let cpu = cpuinfo
        .lines()
        .find_map(|l| l.strip_prefix("model name"))
        .map(|l| l.trim_start_matches([' ', '\t', ':']).to_owned());
    let memory = std::fs::read_to_string("/proc/meminfo")
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|l| l.trim().trim_end_matches(" kB").parse::<f64>().ok())
        .map(|kb| (kb / 1024.0 / 1024.0 * 10.0).round() / 10.0);
    json!({
        "cpu": cpu,
        "cores": std::thread::available_parallelism().map(|n| n.get()).ok(),
        "memory_gib": memory,
        "os": command("uname", &["-sr"]),
    })
}

fn main() {
    let a = args();
    std::fs::create_dir_all(&a.work).expect("the work directory");
    let mut datasets = Vec::new();
    for &scale in &a.scales {
        eprintln!("scale {scale}: generating");
        let triples = generate::generate(scale);
        let dir = a.work.join(format!("lubm-{scale}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the dataset directory");
        let written = layouts::write_all(&dir, &triples, a.runs);
        eprintln!(
            "scale {scale}: {} triples written five ways; accessing",
            triples.len()
        );
        let access = queries::run_all(&dir, &written, a.runs);
        datasets.push(json!({
            "dataset": "lubm-synthetic",
            "scale": scale,
            "triples": written.triples,
            "gar": written.gar_summary,
            "size_bytes": written.sizes,
            "write_seconds": written.write_seconds,
            "stats_share": written.stats_share,
            "load_seconds": access.load_seconds,
            "access_seconds": access.seconds,
            "access_rows": access.rows,
        }));
    }
    let results = json!({
        "generated_by": "cargo run --release -p graphar-bench",
        "commit": command("git", &["rev-parse", "HEAD"]),
        "dirty": !command("git", &["status", "--porcelain", "--untracked-files=no"]).is_empty(),
        "date": command("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]),
        "host": host(),
        "runs": a.runs,
        "gar_chunk_size": layouts::CHUNK_SIZE,
        "timing": "median of `runs` runs after one unmeasured run; local files",
        "versions": {
            "duckdb": queries::duckdb_version(),
            "hdt": "hdt 0.7 (Rust)",
            "parquet": "parquet 58 (Rust), zstd level 3 for GAR and SPO",
        },
        "datasets": datasets,
    });
    std::fs::write(
        &a.out,
        serde_json::to_string_pretty(&results).expect("JSON") + "\n",
    )
    .expect("the results file");
    eprintln!("wrote {}", a.out.display());
}
