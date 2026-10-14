//! Every information file of `apache/incubator-graphar-testing` at 8286724, vendored under
//! `tests/fixtures/` (see its README).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use graphar_info::{EdgeInfo, Error, GraphInfo, VertexInfo};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Every `*.yml` under the fixtures, as paths relative to it, sorted.
fn files(suffix: &str) -> Vec<PathBuf> {
    fn walk(dir: &Path, suffix: &str, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, suffix, out);
            } else if path.to_str().unwrap().ends_with(suffix) {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&fixtures(), suffix, &mut out);
    out.sort();
    out
}

/// The fixtures written in keys GraphAr no longer defines, each refused with the key it names
/// rather than read through an alias or with the key dropped.
///
/// - `java/` is in the Java library's earlier keys (`label`, `src_label`, …), which
///   incubator-graphar-testing renamed to `type`, `src_type`, … in 491fce7 everywhere but here.
/// - Three edge documents keep the earlier `property_groups` under each adjacency list. C++ reads
///   past the key, so `person_studyAt_organisation` loads there with no properties at all.
const REFUSED: &[(&str, &str)] = &[
    ("java/", "label`, expected one of `"),
    ("ldbc/ldbc.graph.yml", "unknown field `property_groups`"),
    (
        "ldbc/person_studyAt_organisation.edge.yml",
        "unknown field `property_groups`",
    ),
    (
        "ldbc/person_workAt_organisation.edge.yml",
        "unknown field `property_groups`",
    ),
    (
        "nebula/basketballplayergraph.graph.yml",
        "unknown field `property_groups`",
    ),
    (
        "nebula/player_serve_team.edge.yml",
        "unknown field `property_groups`",
    ),
];

fn refusal(path: &Path) -> Option<&'static str> {
    let rel = path.strip_prefix(fixtures()).unwrap().to_str().unwrap();
    REFUSED
        .iter()
        .find(|(p, _)| rel.starts_with(p))
        .map(|(_, why)| *why)
}

/// Load, dump, load the dump: the model survives, and dumping it again is byte-identical.
fn round_trip(graph: &GraphInfo) {
    let files = graph.dump().unwrap();
    let written: HashMap<_, _> = files.iter().cloned().collect();
    let (graph_file, graph_yaml) = files.last().unwrap();
    assert_eq!(graph_file, &graph.file_name());
    let again = GraphInfo::load(graph_yaml, |name| Ok(written[name].clone())).unwrap();
    assert_eq!(&again, graph);
    assert_eq!(again.dump().unwrap(), files);
}

#[test]
fn every_graph_loads_validates_and_round_trips() {
    let graphs: Vec<_> = files(".graph.yml")
        .into_iter()
        .filter(|p| refusal(p).is_none())
        .collect();
    assert_eq!(graphs.len(), 11, "{graphs:?}");
    for path in graphs {
        let graph =
            GraphInfo::load_file(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        graph
            .validate()
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        round_trip(&graph);
    }
}

#[test]
fn every_vertex_and_edge_document_round_trips() {
    for path in files(".vertex.yml")
        .into_iter()
        .filter(|p| refusal(p).is_none())
    {
        let v = VertexInfo::load(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            VertexInfo::load(&v.dump().unwrap()).unwrap(),
            v,
            "{}",
            path.display()
        );
    }
    for path in files(".edge.yml")
        .into_iter()
        .filter(|p| refusal(p).is_none())
    {
        let e = EdgeInfo::load(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            EdgeInfo::load(&e.dump().unwrap()).unwrap(),
            e,
            "{}",
            path.display()
        );
    }
}

#[test]
fn documents_in_keys_graphar_no_longer_defines_are_refused() {
    let mut refused = 0;
    for path in files(".yml") {
        let Some(why) = refusal(&path) else { continue };
        let err = if path.to_str().unwrap().ends_with(".graph.yml") {
            GraphInfo::load_file(&path).unwrap_err()
        } else if path.to_str().unwrap().ends_with(".vertex.yml") {
            VertexInfo::load(&std::fs::read_to_string(&path).unwrap()).unwrap_err()
        } else {
            EdgeInfo::load(&std::fs::read_to_string(&path).unwrap()).unwrap_err()
        };
        assert!(matches!(err, Error::Yaml { .. }), "{err}");
        assert!(err.to_string().contains(why), "{}: {err}", path.display());
        refused += 1;
    }
    assert_eq!(refused, 9 + 5);
}

#[test]
fn absent_keys_take_graphars_defaults() {
    let graph =
        GraphInfo::load_file(fixtures().join("ldbc_sample/parquet/ldbc_sample.graph.yml")).unwrap();
    assert_eq!(graph.prefix, None);
    let person = &graph.vertices[0];
    assert_eq!(person.property_groups[0].prefix, "id/");
    assert_eq!(
        person.property_groups[1].prefix,
        "firstName_lastName_gender/"
    );
    assert!(
        !person.property_groups[0].properties[0].is_nullable,
        "a primary key"
    );
    assert!(person.property_groups[1].properties[0].is_nullable);
    let knows = &graph.edges[0];
    let prefixes: Vec<_> = knows.adj_lists.iter().map(|a| a.prefix.as_str()).collect();
    assert_eq!(
        prefixes,
        [
            "unordered_by_source/",
            "ordered_by_source/",
            "ordered_by_dest/"
        ]
    );
}
