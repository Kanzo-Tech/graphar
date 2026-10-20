//! A GraphAr graph and its schema back to triples: the mapping, evaluated natively.

use arrow_array::cast::AsArray;
use arrow_array::types::{Date32Type, Int32Type, Int64Type};
use arrow_array::{Array, ArrayRef, RecordBatch};
use arrow_schema::DataType as Arrow;
use graphar_info::{GraphInfo, VertexInfo};
use graphar_parquet::{
    DST_INDEX_COL, SRC_INDEX_COL, paths, read_adj_list_chunk, read_count, read_vertex_chunk,
};
use object_store::{ObjectStore, path::Path};
use oxrdf::{BlankNode, Literal, NamedNode, NamedOrBlankNode, Term, Triple};

use crate::schema::{Error, Object, RESIDUAL, Schema};
use crate::term::civil;
use crate::{RDF_TYPE, SUBJECT, XSD_STRING};

/// **The triples of the graph under `root`**, as `mapping.ttl` reads them: for each vertex, its
/// `rdf:type` and a triple per value of each column; for each edge, the predicate between the
/// subjects of its ends; and each row of the residual table.
///
/// # Errors
/// What the store or the readers refuse, and a table that does not hold what `info` says.
pub async fn read(
    store: &dyn ObjectStore,
    root: &Path,
    info: &GraphInfo,
    schema: &Schema,
) -> Result<Vec<Triple>, Error> {
    let mut out = Vec::new();
    let rdf_type = NamedNode::new_unchecked(RDF_TYPE);
    let mut subjects: Vec<Vec<NamedNode>> = Vec::with_capacity(schema.vertices.len());
    for vertex in &schema.vertices {
        let vi = vertex_info(info, &vertex.label)?;
        let table = table(store, root, vi).await?;
        let s = named(
            table
                .column_by_name(SUBJECT)
                .ok_or_else(|| missing(&vertex.label, SUBJECT))?,
        )?;
        if let Some(class) = &vertex.class {
            let class = NamedNode::new(class).map_err(|_| Error::NotAnIri(class.clone()))?;
            out.extend(
                s.iter()
                    .map(|s| Triple::new(s.clone(), rdf_type.clone(), class.clone())),
            );
        }
        for column in &vertex.columns {
            let array = table
                .column_by_name(&column.name)
                .ok_or_else(|| missing(&vertex.label, &column.name))?;
            let predicate = NamedNode::new(&column.predicate)
                .map_err(|_| Error::NotAnIri(column.predicate.clone()))?;
            for (row, subject) in s.iter().enumerate() {
                for object in objects(&column.object, array, row)? {
                    out.push(Triple::new(subject.clone(), predicate.clone(), object));
                }
            }
        }
        subjects.push(s);
    }
    for edge in &schema.edges {
        let ei = info
            .edges
            .iter()
            .find(|e| {
                e.src_type == edge.source
                    && e.edge_type == edge.label
                    && e.dst_type == edge.destination
            })
            .ok_or_else(|| Error::Invalid(format!("no edge type `{}`", edge.triple())))?;
        let at = |label: &str| {
            schema
                .vertices
                .iter()
                .position(|v| v.label == label)
                .expect("checked")
        };
        let (src, dst) = (
            &subjects[at(&edge.source)],
            &subjects[at(&edge.destination)],
        );
        let predicate = NamedNode::new_unchecked(&edge.predicate);
        let adj = &ei.adj_lists[0];
        let parts = read_count(store, root, &paths::edge_vertex_count(ei, adj))
            .await?
            .div_ceil(ei.src_chunk_size);
        for part in 0..parts {
            let n = read_count(store, root, &paths::edge_count(ei, adj, part)).await?;
            for chunk in 0..n.div_ceil(ei.chunk_size) {
                for batch in read_adj_list_chunk(store, root, ei, adj, part, chunk).await? {
                    let ids = |name: &str| -> Result<Vec<i64>, Error> {
                        Ok(batch
                            .column_by_name(name)
                            .ok_or_else(|| missing(&edge.triple(), name))?
                            .as_primitive::<Int64Type>()
                            .values()
                            .to_vec())
                    };
                    for (s, d) in ids(SRC_INDEX_COL)?.into_iter().zip(ids(DST_INDEX_COL)?) {
                        let (s, d) = (index(src, s)?, index(dst, d)?);
                        out.push(Triple::new(s.clone(), predicate.clone(), d.clone()));
                    }
                }
            }
        }
    }
    let table = table(store, root, vertex_info(info, RESIDUAL)?).await?;
    let column = |name: &str| -> Result<&arrow_array::StringArray, Error> {
        Ok(table
            .column_by_name(name)
            .ok_or_else(|| missing(RESIDUAL, name))?
            .as_string::<i32>())
    };
    let (s, p, o, dt, lang) = (
        column(SUBJECT)?,
        column("predicate")?,
        column("object")?,
        column("datatype")?,
        column("lang")?,
    );
    for row in 0..table.num_rows() {
        let subject = match s.value(row).strip_prefix("_:") {
            Some(label) => NamedOrBlankNode::from(blank(label)?),
            None => NamedOrBlankNode::from(iri(s.value(row))?),
        };
        let value = o.value(row);
        let object = if lang.is_valid(row) {
            Term::from(
                Literal::new_language_tagged_literal(value, lang.value(row))
                    .map_err(|e| Error::Invalid(e.to_string()))?,
            )
        } else if dt.is_valid(row) {
            Term::from(Literal::new_typed_literal(value, iri(dt.value(row))?))
        } else if let Some(label) = value.strip_prefix("_:") {
            Term::from(blank(label)?)
        } else {
            Term::from(iri(value)?)
        };
        out.push(Triple::new(subject, iri(p.value(row))?, object));
    }
    Ok(out)
}

fn vertex_info<'a>(info: &'a GraphInfo, label: &str) -> Result<&'a VertexInfo, Error> {
    info.vertices
        .iter()
        .find(|v| v.r#type == label)
        .ok_or_else(|| Error::Invalid(format!("no vertex type `{label}`")))
}

fn missing(at: &str, column: &str) -> Error {
    Error::Invalid(format!("`{at}` has no column `{column}`"))
}

fn iri(text: &str) -> Result<NamedNode, Error> {
    NamedNode::new(text).map_err(|_| Error::NotAnIri(text.to_owned()))
}

fn blank(label: &str) -> Result<BlankNode, Error> {
    BlankNode::new(label).map_err(|e| Error::Invalid(e.to_string()))
}

fn index(subjects: &[NamedNode], id: i64) -> Result<&NamedNode, Error> {
    usize::try_from(id)
        .ok()
        .and_then(|i| subjects.get(i))
        .ok_or_else(|| Error::Invalid(format!("internal id {id} has no vertex")))
}

/// A vertex type's whole table: every chunk of its one property group.
async fn table(
    store: &dyn ObjectStore,
    root: &Path,
    vi: &VertexInfo,
) -> Result<RecordBatch, Error> {
    let count = read_count(store, root, &paths::vertex_count(vi)).await?;
    let group = &vi.property_groups[0];
    let mut batches = Vec::new();
    for chunk in 0..count.div_ceil(vi.chunk_size) {
        batches.extend(read_vertex_chunk(store, root, vi, group, chunk).await?);
    }
    match batches.first() {
        Some(first) => Ok(arrow_select_concat(&first.schema(), &batches)?),
        None => Err(Error::Invalid(format!("`{}` has no chunk", vi.r#type))),
    }
}

fn arrow_select_concat(
    schema: &arrow_schema::SchemaRef,
    batches: &[RecordBatch],
) -> Result<RecordBatch, Error> {
    // One batch per chunk; a table of one chunk needs no copy.
    if batches.len() == 1 {
        return Ok(batches[0].clone());
    }
    let columns = (0..schema.fields().len())
        .map(|i| {
            let arrays: Vec<&dyn Array> = batches.iter().map(|b| b.column(i).as_ref()).collect();
            arrow_select::concat::concat(&arrays)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RecordBatch::try_new(schema.clone(), columns)?)
}

fn named(array: &ArrayRef) -> Result<Vec<NamedNode>, Error> {
    let text = arrow_cast::cast(array, &Arrow::Utf8)?;
    text.as_string::<i32>()
        .iter()
        .map(|s| iri(s.unwrap_or_default()))
        .collect()
}

/// The objects of `array` at `row`: none for a null, each element of a list.
fn objects(kind: &Object, array: &ArrayRef, row: usize) -> Result<Vec<Term>, Error> {
    if array.is_null(row) {
        return Ok(Vec::new());
    }
    let (values, range) = match array.data_type() {
        Arrow::List(_) => {
            let list = array.as_list::<i32>();
            let offsets = list.value_offsets();
            (
                list.values().clone(),
                offsets[row] as usize..offsets[row + 1] as usize,
            )
        }
        _ => (array.clone(), row..row + 1),
    };
    range
        .filter(|&i| values.is_valid(i))
        .map(|i| term(kind, &values, i))
        .collect()
}

fn term(kind: &Object, values: &ArrayRef, i: usize) -> Result<Term, Error> {
    if let Arrow::Struct(_) = values.data_type() {
        let s = values.as_struct();
        let (form, tag) = (
            s.column(0).as_string::<i32>(),
            s.column(1).as_string::<i32>(),
        );
        return Ok(Term::from(
            Literal::new_language_tagged_literal(form.value(i), tag.value(i))
                .map_err(|e| Error::Invalid(e.to_string()))?,
        ));
    }
    let lexical = match values.data_type() {
        Arrow::Utf8 => values.as_string::<i32>().value(i).to_owned(),
        Arrow::Int64 => values.as_primitive::<Int64Type>().value(i).to_string(),
        Arrow::Int32 => values.as_primitive::<Int32Type>().value(i).to_string(),
        Arrow::Boolean => values.as_boolean().value(i).to_string(),
        Arrow::Date32 => civil(values.as_primitive::<Date32Type>().value(i)),
        other => return Err(Error::Invalid(format!("a column of {other}"))),
    };
    Ok(match kind {
        Object::Iri => Term::from(iri(&lexical)?),
        Object::Literal(dt) if dt == XSD_STRING => Term::from(Literal::new_simple_literal(lexical)),
        Object::Literal(dt) => Term::from(Literal::new_typed_literal(lexical, iri(dt)?)),
        Object::LangString => {
            return Err(Error::Invalid(
                "a tagged column that is not a struct".into(),
            ));
        }
    })
}
