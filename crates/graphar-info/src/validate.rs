//! The rules a reader relies on. Each is the C++ library's `IsValidated` (`graph_info.cc`), except
//! where the specification says otherwise; the exceptions are named where they are checked.

use std::collections::HashSet;

use crate::{
    Cardinality, DataType, EdgeInfo, Error, FileType, GraphInfo, PropertyGroup, VertexInfo,
};

fn invalid(at: impl Into<String>, reason: impl Into<String>) -> Error {
    Error::Invalid {
        at: at.into(),
        reason: reason.into(),
    }
}

pub(crate) fn graph(g: &GraphInfo) -> Result<(), Error> {
    let at = format!("graph `{}`", g.name);
    if g.name.is_empty() {
        return Err(invalid("graph", "`name` is empty"));
    }
    if g.prefix.as_deref() == Some("") {
        return Err(invalid(&at, "`prefix` is empty"));
    }
    let mut types = HashSet::new();
    for v in &g.vertices {
        vertex(v)?;
        if !types.insert(v.r#type.as_str()) {
            return Err(invalid(
                &at,
                format!("vertex type `{}` is listed twice", v.r#type),
            ));
        }
    }
    let mut triples = HashSet::new();
    for e in &g.edges {
        edge(e)?;
        if !triples.insert(e.triple()) {
            return Err(invalid(
                &at,
                format!("edge type `{}` is listed twice", e.triple()),
            ));
        }
        // Not checked by C++. An edge's ids are internal vertex ids of its end types, and "the
        // partition of the offset table should be in alignment with the partition of the
        // corresponding vertex table" (format.md, "Physical table of edges").
        for (end, name, chunk_size) in [
            ("src", &e.src_type, e.src_chunk_size),
            ("dst", &e.dst_type, e.dst_chunk_size),
        ] {
            let at_e = format!("edge `{}`", e.triple());
            match g.vertices.iter().find(|v| &v.r#type == name) {
                None => {
                    return Err(invalid(
                        at_e,
                        format!("`{end}_type` `{name}` is not a vertex type of the graph"),
                    ));
                }
                Some(v) if v.chunk_size != chunk_size => {
                    return Err(invalid(
                        at_e,
                        format!(
                            "`{end}_chunk_size` is {chunk_size}, and vertex `{name}`'s `chunk_size` is {}",
                            v.chunk_size
                        ),
                    ));
                }
                Some(_) => {}
            }
        }
    }
    let mut keys = HashSet::new();
    for (key, _) in &g.extra_info {
        // C++ reads `extra_info` into a map, so a repeated key would lose a value silently.
        if !keys.insert(key.as_str()) {
            return Err(invalid(
                &at,
                format!("`extra_info` key `{key}` is repeated"),
            ));
        }
    }
    Ok(())
}

pub(crate) fn vertex(v: &VertexInfo) -> Result<(), Error> {
    let at = format!("vertex `{}`", v.r#type);
    if v.r#type.is_empty() {
        return Err(invalid("vertex", "`type` is empty"));
    }
    if v.chunk_size == 0 {
        return Err(invalid(&at, "`chunk_size` is 0"));
    }
    if v.prefix.is_empty() {
        return Err(invalid(&at, "`prefix` is empty"));
    }
    groups(&at, &v.property_groups, &v.version.user_define_types, false)
}

pub(crate) fn edge(e: &EdgeInfo) -> Result<(), Error> {
    let at = format!("edge `{}`", e.triple());
    if e.src_type.is_empty() || e.edge_type.is_empty() || e.dst_type.is_empty() {
        return Err(invalid(
            &at,
            "`src_type`, `edge_type` and `dst_type` must all be named",
        ));
    }
    for (key, size) in [
        ("chunk_size", e.chunk_size),
        ("src_chunk_size", e.src_chunk_size),
        ("dst_chunk_size", e.dst_chunk_size),
    ] {
        if size == 0 {
            return Err(invalid(&at, format!("`{key}` is 0")));
        }
    }
    if e.prefix.is_empty() {
        return Err(invalid(&at, "`prefix` is empty"));
    }
    if e.adj_lists.is_empty() {
        return Err(invalid(&at, "`adj_lists` is empty"));
    }
    let mut kinds = HashSet::new();
    for a in &e.adj_lists {
        if !kinds.insert(a.r#type) {
            return Err(invalid(
                &at,
                format!("adjacency list `{}` is listed twice", a.r#type),
            ));
        }
        if a.prefix.is_empty() {
            return Err(invalid(
                format!("{at}, adjacency list `{}`", a.r#type),
                "`prefix` is empty",
            ));
        }
    }
    groups(&at, &e.property_groups, &e.version.user_define_types, true)
}

fn groups(
    at: &str,
    groups: &[PropertyGroup],
    declared: &[String],
    edge: bool,
) -> Result<(), Error> {
    let mut names = HashSet::new();
    for (i, g) in groups.iter().enumerate() {
        let at_group = format!("{at}, property group {i}");
        // C++'s `IsValidated` also refuses `json`; the specification lists JSON among the file
        // types (format.md, "Data File Format"), and the specification wins.
        if g.prefix.is_empty() {
            return Err(invalid(&at_group, "`prefix` is empty"));
        }
        if g.properties.is_empty() {
            return Err(invalid(&at_group, "`properties` is empty"));
        }
        for p in &g.properties {
            let at_p = format!("{at}, property `{}`", p.name);
            if p.name.is_empty() {
                return Err(invalid(&at_group, "a property's `name` is empty"));
            }
            if !names.insert(p.name.as_str()) {
                return Err(invalid(&at_p, "the name is used twice in this type"));
            }
            if p.is_primary && p.is_nullable {
                return Err(invalid(&at_p, "a primary key cannot be nullable"));
            }
            if let Some(name) = undeclared(&p.data_type, declared) {
                return Err(invalid(
                    &at_p,
                    format!(
                        "data type `{name}` is not GraphAr's, and the version does not declare it"
                    ),
                ));
            }
            if edge && p.cardinality != Cardinality::Single {
                return Err(invalid(
                    &at_p,
                    "an edge property's cardinality must be `single`",
                ));
            }
            if g.file_type == FileType::Csv {
                if matches!(p.data_type, DataType::List(_)) {
                    return Err(invalid(&at_p, "a `list` type cannot be stored in `csv`"));
                }
                if p.cardinality != Cardinality::Single {
                    return Err(invalid(
                        &at_p,
                        "a cardinality other than `single` cannot be stored in `csv`",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn undeclared<'a>(t: &'a DataType, declared: &[String]) -> Option<&'a str> {
    match t {
        DataType::List(item) => undeclared(item, declared),
        DataType::UserDefined(name) if !declared.contains(name) => Some(name),
        _ => None,
    }
}
