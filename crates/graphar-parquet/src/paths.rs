//! Where each data file is, relative to the graph's root. These are conventions of GraphAr's code,
//! not of its specification, and each function names the C++ `graph_info.cc` method it follows.
//!
//! A prefix is joined as C++'s `BuildPath` joins it: as written, with a slash added when it does
//! not end in one.

use graphar_info::{AdjacentList, EdgeInfo, PropertyGroup, VertexInfo};

fn dir(parts: &[&str]) -> String {
    let mut path = String::new();
    for p in parts {
        path.push_str(p);
        if !p.ends_with('/') {
            path.push('/');
        }
    }
    path
}

/// Chunk `chunk` of a vertex property group: `<vertex prefix><group prefix>chunk<k>`.
/// C++ `VertexInfo::GetFilePath`.
pub fn vertex_chunk(info: &VertexInfo, group: &PropertyGroup, chunk: u64) -> String {
    format!("{}chunk{chunk}", dir(&[&info.prefix, &group.prefix]))
}

/// The vertex type's count file: `<vertex prefix>vertex_count`.
/// C++ `VertexInfo::GetVerticesNumFilePath`.
pub fn vertex_count(info: &VertexInfo) -> String {
    format!("{}vertex_count", dir(&[&info.prefix]))
}

/// Chunk `chunk` of part `part` of an adjacency list:
/// `<edge prefix><list prefix>adj_list/part<p>/chunk<k>`. A part holds the edges whose source
/// (or destination, for a list aligned by it) is in vertex chunk `part`.
/// C++ `EdgeInfo::GetAdjListFilePath`.
pub fn adj_list_chunk(info: &EdgeInfo, adj: &AdjacentList, part: u64, chunk: u64) -> String {
    format!(
        "{}adj_list/part{part}/chunk{chunk}",
        dir(&[&info.prefix, &adj.prefix])
    )
}

/// Chunk `chunk` of part `part` of an edge property group, row for row beside the adjacency
/// list's: `<edge prefix><list prefix><group prefix>part<p>/chunk<k>`.
/// C++ `EdgeInfo::GetPropertyFilePath`.
pub fn edge_property_chunk(
    info: &EdgeInfo,
    adj: &AdjacentList,
    group: &PropertyGroup,
    part: u64,
    chunk: u64,
) -> String {
    format!(
        "{}part{part}/chunk{chunk}",
        dir(&[&info.prefix, &adj.prefix, &group.prefix])
    )
}

/// The offset chunk of part `part` of an ordered adjacency list:
/// `<edge prefix><list prefix>offset/chunk<p>`. C++ `EdgeInfo::GetAdjListOffsetFilePath`.
pub fn offset_chunk(info: &EdgeInfo, adj: &AdjacentList, part: u64) -> String {
    format!("{}offset/chunk{part}", dir(&[&info.prefix, &adj.prefix]))
}

/// The edge count of part `part` of an adjacency list: `<edge prefix><list prefix>edge_count<p>`.
/// C++ `EdgeInfo::GetEdgesNumFilePath`.
pub fn edge_count(info: &EdgeInfo, adj: &AdjacentList, part: u64) -> String {
    format!("{}edge_count{part}", dir(&[&info.prefix, &adj.prefix]))
}

/// The count of the vertices an adjacency list is aligned by — the sources for a list by source,
/// the destinations for one by destination: `<edge prefix><list prefix>vertex_count`.
/// C++ `EdgeInfo::GetVerticesNumFilePath`.
pub fn edge_vertex_count(info: &EdgeInfo, adj: &AdjacentList) -> String {
    format!("{}vertex_count", dir(&[&info.prefix, &adj.prefix]))
}
