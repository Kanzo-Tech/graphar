//! The six accesses, on every layout that has an access path, and the time to load N-Triples.

use std::fmt::Write as _;
use std::path::Path;

use duckdb::Connection;
use graphar_rdf::{Cardinality, Object, RESIDUAL, Schema, Storage};
use serde_json::{Map, Value, json};

use crate::layouts::Written;
use crate::timed;

const UB: &str = "http://lubm.example.org/";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// The times and the row counts of every access on every layout.
pub struct Access {
    pub load_seconds: Value,
    pub seconds: Value,
    pub rows: Value,
}

/// One access, as each layout asks it.
struct Query {
    name: &'static str,
    /// On SPO's table `spo` and on GAR's view `rdf`, both `(subject, predicate, object)`.
    triples_sql: String,
    /// On GAR's own tables, where the access is better asked of them than of the view.
    gar_sql: Option<String>,
    /// As an HDT triple pattern, and a filter on the objects it returns.
    hdt: (Option<String>, Option<String>, Option<String>),
    hdt_filter: Option<fn(&str) -> bool>,
}

fn queries() -> Vec<Query> {
    let professor = format!("{UB}University0/Department0/Professor3");
    let student = format!("{UB}University0/Department0/Student7");
    let course = format!("{UB}University0/Department0/Course5");
    let takes = format!("{UB}takesCourse");
    let name = format!("{UB}name");
    let enrolled = format!("{UB}enrolled");
    vec![
        Query {
            name: "subject",
            triples_sql: format!("SELECT count(*) FROM rdf WHERE subject = '{professor}'"),
            gar_sql: None,
            hdt: (Some(professor.clone()), None, None),
            hdt_filter: None,
        },
        Query {
            name: "predicate",
            triples_sql: format!("SELECT count(*) FROM rdf WHERE predicate = '{takes}'"),
            gar_sql: None,
            hdt: (None, Some(takes.clone()), None),
            hdt_filter: None,
        },
        Query {
            name: "predicate_object",
            triples_sql: format!(
                "SELECT count(*) FROM rdf WHERE predicate = '{name}' AND object = 'Course 5'"
            ),
            gar_sql: None,
            hdt: (None, Some(name.clone()), Some("\"Course 5\"".to_owned())),
            hdt_filter: None,
        },
        Query {
            name: "date_range",
            // SPO's dates are text, which for `YYYY-MM-DD` compares as the dates do.
            triples_sql: format!(
                "SELECT count(*) FROM rdf WHERE predicate = '{enrolled}' \
                 AND object BETWEEN '2015-01-01' AND '2015-12-31'"
            ),
            gar_sql: Some(
                "SELECT count(*) FROM \"Student\" \
                 WHERE \"enrolled\" BETWEEN DATE '2015-01-01' AND DATE '2015-12-31'"
                    .to_owned(),
            ),
            hdt: (None, Some(enrolled.clone()), None),
            hdt_filter: Some(|o| {
                let lexical = o.trim_start_matches('"').split('"').next().unwrap_or("");
                ("2015-01-01"..="2015-12-31").contains(&lexical)
            }),
        },
        Query {
            name: "out_neighbourhood",
            triples_sql: format!(
                "SELECT count(*) FROM rdf WHERE subject = '{student}' AND predicate = '{takes}'"
            ),
            gar_sql: None,
            hdt: (Some(student.clone()), Some(takes.clone()), None),
            hdt_filter: None,
        },
        Query {
            name: "in_neighbourhood",
            triples_sql: format!(
                "SELECT count(*) FROM rdf WHERE predicate = '{takes}' AND object = '{course}'"
            ),
            gar_sql: None,
            hdt: (None, Some(takes), Some(course)),
            hdt_filter: None,
        },
    ]
}

/// DuckDB's version, as the engine reports it.
pub fn duckdb_version() -> String {
    Connection::open_in_memory()
        .and_then(|c| c.query_row("SELECT version()", [], |r| r.get::<_, String>(0)))
        .unwrap_or_default()
}

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn lit(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// GAR's tables as DuckDB views over their chunks, and `rdf`, the RDF reading as one view (the
/// residual table is `triples`): the mapping's triples maps, each a branch of a `UNION ALL`, every
/// object as text. With the SQL, each branch and the predicate it reads, `None` for the residual
/// table's, which may hold any.
fn gar_views(dir: &Path, schema: &Schema) -> (String, Vec<(Option<String>, String)>) {
    let mut sql = String::new();
    let glob = |p: String| lit(&dir.join(p).display().to_string());
    let labels = schema
        .vertices
        .iter()
        .map(|v| v.label.as_str())
        .chain([RESIDUAL]);
    for label in labels {
        writeln!(
            sql,
            "CREATE VIEW {} AS SELECT * FROM read_parquet({});",
            quoted(label),
            glob(format!("vertex/{label}/properties/chunk*"))
        )
        .expect("a String");
    }
    for e in &schema.edges {
        writeln!(
            sql,
            "CREATE VIEW {} AS SELECT * FROM read_parquet({});",
            quoted(&e.triple()),
            glob(format!(
                "edge/{}/ordered_by_source/adj_list/part*/chunk*",
                e.triple()
            ))
        )
        .expect("a String");
    }
    let mut branches = Vec::new();
    for v in &schema.vertices {
        let t = quoted(&v.label);
        if let Some(class) = &v.class {
            branches.push((
                Some(RDF_TYPE.to_owned()),
                format!(
                    "SELECT subject, {} AS predicate, {} AS object FROM {t}",
                    lit(RDF_TYPE),
                    lit(class)
                ),
            ));
        }
        for c in &v.columns {
            let col = quoted(&c.name);
            let tagged = c.storage() == Storage::LangString || c.object == Object::LangString;
            let value = if tagged {
                "(v).\"value\""
            } else {
                "CAST(v AS VARCHAR)"
            };
            let rows = match c.cardinality {
                Cardinality::Set => format!("SELECT subject, unnest({col}) AS v FROM {t}"),
                Cardinality::Single => format!("SELECT subject, {col} AS v FROM {t}"),
            };
            branches.push((
                Some(c.predicate.clone()),
                format!(
                    "SELECT subject, {} AS predicate, {value} AS object FROM ({rows}) WHERE v IS NOT NULL",
                    lit(&c.predicate)
                ),
            ));
        }
    }
    for e in &schema.edges {
        branches.push((
            Some(e.predicate.clone()),
            format!(
                "SELECT s.subject, {} AS predicate, d.subject AS object FROM {} AS e \
                 JOIN {} AS s ON e._graphArSrcIndex = s._graphArVertexIndex \
                 JOIN {} AS d ON e._graphArDstIndex = d._graphArVertexIndex",
                lit(&e.predicate),
                quoted(&e.triple()),
                quoted(&e.source),
                quoted(&e.destination)
            ),
        ));
    }
    branches.push((
        None,
        format!(
            "SELECT subject, predicate, object FROM {}",
            quoted(RESIDUAL)
        ),
    ));
    writeln!(sql, "CREATE VIEW rdf AS {};", union(&branches, None)).expect("a String");
    (sql, branches)
}

/// The branches that can hold `predicate` (all of them for none), as one `UNION ALL`: what a
/// reader that has the mapping reads for a pattern whose predicate is bound.
fn union(branches: &[(Option<String>, String)], predicate: Option<&str>) -> String {
    branches
        .iter()
        .filter(|(p, _)| predicate.is_none() || p.is_none() || p.as_deref() == predicate)
        .map(|(_, sql)| sql.as_str())
        .collect::<Vec<_>>()
        .join("\nUNION ALL\n")
}

fn count(con: &Connection, sql: &str) -> u64 {
    con.query_row(sql, [], |r| r.get::<_, i64>(0))
        .unwrap_or_else(|e| panic!("{sql}: {e}")) as u64
}

/// Every access on SPO, GAR and HDT, and N-Triples' and HDT's load times.
pub fn run_all(_dir: &Path, w: &Written, runs: usize) -> Access {
    let spo = Connection::open_in_memory().expect("DuckDB");
    spo.execute_batch(&format!(
        "CREATE VIEW rdf AS SELECT * FROM read_parquet({});",
        lit(&w.spo.display().to_string())
    ))
    .expect("the SPO view");
    let gar = Connection::open_in_memory().expect("DuckDB");
    let (setup, branches) = gar_views(&w.gar, &w.schema);
    gar.execute_batch(&setup).expect("the GAR views");

    let (nt_seconds, nt_count) = timed(runs, || {
        let bytes = std::fs::read(&w.nt).expect("NT");
        oxttl::NTriplesParser::new().for_slice(&bytes).count()
    });
    assert_eq!(nt_count, w.triples, "N-Triples parses to every triple");
    let (hdt_load, h) = timed(runs, || {
        let file = std::fs::File::open(&w.hdt).expect("HDT");
        hdt::Hdt::read(std::io::BufReader::new(file)).expect("HDT read")
    });
    // The whole graph, every layout: the counts must be the triples.
    assert_eq!(
        count(&spo, "SELECT count(*) FROM rdf") as usize,
        w.triples,
        "SPO"
    );
    assert_eq!(
        count(&gar, "SELECT count(*) FROM rdf") as usize,
        w.triples,
        "GAR"
    );

    let mut seconds = Map::new();
    let mut rows = Map::new();
    for q in queries() {
        let (spo_s, spo_n) = timed(runs, || count(&spo, &q.triples_sql));
        // GAR twice: through the view of every triples map, and through the ones that can hold
        // the bound predicate (or the typed table, for the date range).
        let (view_s, view_n) = timed(runs, || count(&gar, &q.triples_sql));
        let routed = q.gar_sql.clone().unwrap_or_else(|| match &q.hdt.1 {
            Some(p) => q.triples_sql.replace(
                "FROM rdf",
                &format!("FROM ({}) AS rdf", union(&branches, Some(p))),
            ),
            None => q.triples_sql.clone(),
        });
        let (gar_s, gar_n) = timed(runs, || count(&gar, &routed));
        let (hdt_s, hdt_n) = timed(runs, || {
            let (s, p, o) = &q.hdt;
            h.triples_with_pattern(s.as_deref(), p.as_deref(), o.as_deref())
                .filter(|t| q.hdt_filter.is_none_or(|f| f(t[2].as_ref())))
                .count() as u64
        });
        assert!(
            spo_n == gar_n && gar_n == hdt_n && view_n == gar_n,
            "{}: SPO {spo_n}, GAR {gar_n} (view {view_n}), HDT {hdt_n}",
            q.name
        );
        seconds.insert(
            q.name.to_owned(),
            json!({"spo": spo_s, "gar": gar_s, "gar_view": view_s, "hdt": hdt_s}),
        );
        rows.insert(q.name.to_owned(), json!(gar_n));
    }
    Access {
        load_seconds: json!({"nt_parse": nt_seconds, "hdt_open": hdt_load}),
        seconds: Value::Object(seconds),
        rows: Value::Object(rows),
    }
}
