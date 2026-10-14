//! The documents as they are written. Each struct's field order is the order the C++ library's
//! `Dump` writes the keys in, so a dump reads like GraphAr's own. A key GraphAr does not define is
//! refused, never dropped: the model carries everything the document says.

use serde::{Deserialize, Serialize};

use crate::{
    AdjListType, AdjacentList, Cardinality, DataType, EdgeInfo, Error, FileType, GraphInfo,
    InfoVersion, Property, PropertyGroup, VertexInfo,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Graph {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    vertices: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    edges: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    labels: Vec<String>,
    #[serde(with = "text")]
    version: InfoVersion,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    extra_info: Vec<KeyValue>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyValue {
    key: String,
    value: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Vertex {
    r#type: String,
    chunk_size: u64,
    #[serde(default)]
    prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    property_groups: Vec<Group>,
    #[serde(with = "text")]
    version: InfoVersion,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Edge {
    src_type: String,
    edge_type: String,
    dst_type: String,
    chunk_size: u64,
    src_chunk_size: u64,
    dst_chunk_size: u64,
    #[serde(default)]
    prefix: Option<String>,
    #[serde(default)]
    directed: bool,
    #[serde(default)]
    adj_lists: Vec<AdjList>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    property_groups: Vec<Group>,
    #[serde(with = "text")]
    version: InfoVersion,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdjList {
    ordered: bool,
    aligned_by: AlignedBy,
    #[serde(default)]
    prefix: Option<String>,
    #[serde(with = "text")]
    file_type: FileType,
}

#[derive(Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum AlignedBy {
    Src,
    Dst,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Group {
    #[serde(default)]
    prefix: Option<String>,
    #[serde(with = "text")]
    file_type: FileType,
    properties: Vec<Prop>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prop {
    name: String,
    #[serde(with = "text")]
    data_type: DataType,
    #[serde(default)]
    is_primary: bool,
    #[serde(default)]
    is_nullable: Option<bool>,
    #[serde(default, with = "text", skip_serializing_if = "is_single")]
    cardinality: Cardinality,
}

fn is_single(c: &Cardinality) -> bool {
    *c == Cardinality::Single
}

/// A value written as its `Display` and read through its `FromStr`.
mod text {
    use std::fmt::Display;
    use std::str::FromStr;

    use serde::{Deserialize, Deserializer, Serializer, de};

    pub fn serialize<T: Display, S: Serializer>(value: &T, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(value)
    }

    pub fn deserialize<'de, T, D>(d: D) -> Result<T, D::Error>
    where
        T: FromStr<Err = String>,
        D: Deserializer<'de>,
    {
        String::deserialize(d)?.parse().map_err(de::Error::custom)
    }
}

fn parse<'a, T: Deserialize<'a>>(yaml: &'a str, document: &str) -> Result<T, Error> {
    serde_yaml_ng::from_str(yaml).map_err(|source| Error::Yaml {
        document: document.to_owned(),
        source,
    })
}

fn write<T: Serialize>(doc: &T, document: &str) -> Result<String, Error> {
    serde_yaml_ng::to_string(doc).map_err(|source| Error::Yaml {
        document: document.to_owned(),
        source,
    })
}

pub(crate) fn load_graph(
    yaml: &str,
    read: &mut dyn FnMut(&str) -> Result<String, Error>,
) -> Result<GraphInfo, Error> {
    let doc: Graph = parse(yaml, "graph info")?;
    let vertices = doc
        .vertices
        .iter()
        .map(|path| load_vertex(&read(path)?, path))
        .collect::<Result<_, _>>()?;
    let edges = doc
        .edges
        .iter()
        .map(|path| load_edge(&read(path)?, path))
        .collect::<Result<_, _>>()?;
    Ok(GraphInfo {
        name: doc.name,
        prefix: doc.prefix,
        vertices,
        edges,
        labels: doc.labels,
        version: doc.version,
        extra_info: doc
            .extra_info
            .into_iter()
            .map(|kv| (kv.key, kv.value))
            .collect(),
    })
}

pub(crate) fn dump_graph(graph: &GraphInfo) -> Result<Vec<(String, String)>, Error> {
    let mut files = Vec::with_capacity(graph.vertices.len() + graph.edges.len() + 1);
    for v in &graph.vertices {
        files.push((v.file_name(), dump_vertex(v)?));
    }
    for e in &graph.edges {
        files.push((e.file_name(), dump_edge(e)?));
    }
    let doc = Graph {
        name: graph.name.clone(),
        prefix: graph.prefix.clone(),
        vertices: graph.vertices.iter().map(VertexInfo::file_name).collect(),
        edges: graph.edges.iter().map(EdgeInfo::file_name).collect(),
        labels: graph.labels.clone(),
        version: graph.version.clone(),
        extra_info: graph
            .extra_info
            .iter()
            .map(|(key, value)| KeyValue {
                key: key.clone(),
                value: value.clone(),
            })
            .collect(),
    };
    files.push((graph.file_name(), write(&doc, "graph info")?));
    Ok(files)
}

pub(crate) fn load_vertex(yaml: &str, document: &str) -> Result<VertexInfo, Error> {
    let doc: Vertex = parse(yaml, document)?;
    Ok(VertexInfo {
        prefix: doc.prefix.unwrap_or_else(|| format!("{}/", doc.r#type)),
        r#type: doc.r#type,
        chunk_size: doc.chunk_size,
        labels: doc.labels,
        property_groups: doc.property_groups.into_iter().map(group).collect(),
        version: doc.version,
    })
}

pub(crate) fn dump_vertex(v: &VertexInfo) -> Result<String, Error> {
    let doc = Vertex {
        r#type: v.r#type.clone(),
        chunk_size: v.chunk_size,
        prefix: Some(v.prefix.clone()),
        labels: v.labels.clone(),
        property_groups: v.property_groups.iter().map(group_doc).collect(),
        version: v.version.clone(),
    };
    write(&doc, &v.file_name())
}

pub(crate) fn load_edge(yaml: &str, document: &str) -> Result<EdgeInfo, Error> {
    let doc: Edge = parse(yaml, document)?;
    Ok(EdgeInfo {
        prefix: doc
            .prefix
            .unwrap_or_else(|| format!("{}_{}_{}/", doc.src_type, doc.edge_type, doc.dst_type)),
        src_type: doc.src_type,
        edge_type: doc.edge_type,
        dst_type: doc.dst_type,
        chunk_size: doc.chunk_size,
        src_chunk_size: doc.src_chunk_size,
        dst_chunk_size: doc.dst_chunk_size,
        directed: doc.directed,
        adj_lists: doc
            .adj_lists
            .into_iter()
            .map(|a| {
                let r#type = AdjListType::of(a.ordered, a.aligned_by == AlignedBy::Src);
                AdjacentList {
                    r#type,
                    file_type: a.file_type,
                    prefix: a.prefix.unwrap_or_else(|| format!("{type}/")),
                }
            })
            .collect(),
        property_groups: doc.property_groups.into_iter().map(group).collect(),
        version: doc.version,
    })
}

pub(crate) fn dump_edge(e: &EdgeInfo) -> Result<String, Error> {
    let doc = Edge {
        src_type: e.src_type.clone(),
        edge_type: e.edge_type.clone(),
        dst_type: e.dst_type.clone(),
        chunk_size: e.chunk_size,
        src_chunk_size: e.src_chunk_size,
        dst_chunk_size: e.dst_chunk_size,
        prefix: Some(e.prefix.clone()),
        directed: e.directed,
        adj_lists: e
            .adj_lists
            .iter()
            .map(|a| AdjList {
                ordered: a.r#type.ordered(),
                aligned_by: if a.r#type.by_source() {
                    AlignedBy::Src
                } else {
                    AlignedBy::Dst
                },
                prefix: Some(a.prefix.clone()),
                file_type: a.file_type,
            })
            .collect(),
        property_groups: e.property_groups.iter().map(group_doc).collect(),
        version: e.version.clone(),
    };
    write(&doc, &e.file_name())
}

fn group(g: Group) -> PropertyGroup {
    let prefix = g.prefix.unwrap_or_else(|| {
        let names: Vec<&str> = g.properties.iter().map(|p| p.name.as_str()).collect();
        format!("{}/", names.join("_"))
    });
    PropertyGroup {
        properties: g
            .properties
            .into_iter()
            .map(|p| Property {
                is_nullable: p.is_nullable.unwrap_or(!p.is_primary),
                name: p.name,
                data_type: p.data_type,
                is_primary: p.is_primary,
                cardinality: p.cardinality,
            })
            .collect(),
        file_type: g.file_type,
        prefix,
    }
}

fn group_doc(g: &PropertyGroup) -> Group {
    Group {
        prefix: Some(g.prefix.clone()),
        file_type: g.file_type,
        properties: g
            .properties
            .iter()
            .map(|p| Prop {
                name: p.name.clone(),
                data_type: p.data_type.clone(),
                is_primary: p.is_primary,
                is_nullable: Some(p.is_nullable),
                cardinality: p.cardinality,
            })
            .collect(),
    }
}
