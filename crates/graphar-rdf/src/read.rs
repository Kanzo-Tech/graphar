//! A GraphAr graph and its schema back to triples: its plan, evaluated natively.

use std::collections::HashMap;

use arrow_array::cast::AsArray;
use arrow_array::types::{Date32Type, Int32Type, Int64Type};
use arrow_array::{Array, ArrayRef, RecordBatch};
use arrow_schema::DataType as Arrow;
use graphar_info::{GraphInfo, VertexInfo};
use graphar_parquet::{
    DST_INDEX_COL, SRC_INDEX_COL, paths, read_adj_list_chunk, read_count, read_vertex_chunk,
};
use object_store::{ObjectStore, path::Path};
use oxrdf::{Literal, NamedNode, Term, Triple};

use crate::plan::{Expr, Source, plan};
use crate::schema::{Error, Schema};
use crate::term::civil;
use crate::{RDF_TYPE, SUBJECT};

/// **The triples of the graph under `root`**: its [`plan`] evaluated over the tables — for each
/// map, each row of its source, and of it the triple of each branch whose three terms are bound.
/// `mapping.ttl` and `mapping.r2rml.ttl` serialize the same plan.
///
/// # Errors
/// What the store or the readers refuse, and a table that does not hold what `info` says.
pub async fn read(
    store: &dyn ObjectStore,
    root: &Path,
    info: &GraphInfo,
    schema: &Schema,
) -> Result<Vec<Triple>, Error> {
    let plan = plan(schema)?;
    let mut tables: HashMap<String, RecordBatch> = HashMap::new();
    let mut out = Vec::new();
    for m in &plan.maps {
        let rows = rows(store, root, info, &mut tables, &m.source).await?;
        let rdf_type = NamedNode::new_unchecked(RDF_TYPE);
        let classes = m
            .classes
            .iter()
            .map(|c| iri(c))
            .collect::<Result<Vec<_>, _>>()?;
        let objects = m
            .objects
            .iter()
            .map(|(p, o)| Ok((iri(p)?, o)))
            .collect::<Result<Vec<_>, Error>>()?;
        for row in 0..rows.len {
            let Some(Term::NamedNode(s)) = rows.term(&m.subject, row)? else {
                continue;
            };
            for c in &classes {
                out.push(Triple::new(s.clone(), rdf_type.clone(), c.clone()));
            }
            for (p, o) in &objects {
                if let Some(o) = rows.term(o, row)? {
                    out.push(Triple::new(s.clone(), p.clone(), o));
                }
            }
        }
    }
    Ok(out)
}

/// A source relation, evaluated: each attribute as the lexical forms of its values, `None` for
/// a null.
struct Rows {
    len: usize,
    attributes: HashMap<String, Vec<Option<String>>>,
}

impl Rows {
    fn get(&self, attribute: &str, row: usize) -> Result<Option<&str>, Error> {
        Ok(self
            .attributes
            .get(attribute)
            .ok_or_else(|| Error::Invalid(format!("no attribute `{attribute}`")))?[row]
            .as_deref())
    }

    /// The term `e` makes of `row`: none when an attribute it reads is null.
    fn term(&self, e: &Expr, row: usize) -> Result<Option<Term>, Error> {
        Ok(match e {
            Expr::Iri(a) => match self.get(a, row)? {
                Some(t) => Some(Term::from(iri(t)?)),
                None => None,
            },
            Expr::Literal {
                attribute,
                datatype,
            } => match (self.get(attribute, row)?, datatype) {
                (Some(t), None) => Some(Term::from(Literal::new_simple_literal(t))),
                (Some(t), Some(dt)) => Some(Term::from(Literal::new_typed_literal(t, iri(dt)?))),
                (None, _) => None,
            },
            Expr::LangLiteral { value, lang } => {
                match (self.get(value, row)?, self.get(lang, row)?) {
                    (Some(v), Some(l)) => Some(Term::from(
                        Literal::new_language_tagged_literal(v, l)
                            .map_err(|e| Error::Invalid(e.to_string()))?,
                    )),
                    _ => None,
                }
            }
        })
    }
}

/// `source` over the graph's tables, read once each.
async fn rows(
    store: &dyn ObjectStore,
    root: &Path,
    info: &GraphInfo,
    tables: &mut HashMap<String, RecordBatch>,
    source: &Source,
) -> Result<Rows, Error> {
    let mut attributes = HashMap::new();
    match source {
        Source::Table { table } => {
            let t = cached(store, root, info, tables, table).await?;
            for (field, array) in t.schema().fields().iter().zip(t.columns()) {
                if !matches!(array.data_type(), Arrow::List(_) | Arrow::Struct(_)) {
                    attributes.insert(field.name().clone(), lexical(array)?);
                }
            }
            Ok(Rows {
                len: t.num_rows(),
                attributes,
            })
        }
        Source::Unnest { table, column, .. } | Source::Fields { table, column } => {
            let t = cached(store, root, info, tables, table).await?;
            let subjects = lexical(
                t.column_by_name(SUBJECT)
                    .ok_or_else(|| missing(table, SUBJECT))?,
            )?;
            let array = t
                .column_by_name(column)
                .ok_or_else(|| missing(table, column))?;
            // A row per element: the list's values and their rows, or the column itself.
            let (values, owners): (ArrayRef, Vec<usize>) = match array.data_type() {
                Arrow::List(_) => {
                    let list = array.as_list::<i32>();
                    let offsets = list.value_offsets();
                    let owners = (0..list.len())
                        .filter(|&r| list.is_valid(r))
                        .flat_map(|r| {
                            (offsets[r] as usize..offsets[r + 1] as usize).map(move |_| r)
                        })
                        .collect::<Vec<_>>();
                    let starts: Vec<usize> = (0..list.len())
                        .filter(|&r| list.is_valid(r))
                        .flat_map(|r| offsets[r] as usize..offsets[r + 1] as usize)
                        .collect();
                    let taken = arrow_select::take::take(
                        list.values().as_ref(),
                        &arrow_array::UInt32Array::from_iter_values(
                            starts.iter().map(|&i| i as u32),
                        ),
                        None,
                    )?;
                    (taken, owners)
                }
                _ => (array.clone(), (0..array.len()).collect()),
            };
            attributes.insert(
                SUBJECT.to_owned(),
                owners.iter().map(|&r| subjects[r].clone()).collect(),
            );
            if let Arrow::Struct(_) = values.data_type() {
                let s = values.as_struct();
                let field = |i: usize| -> Result<Vec<Option<String>>, Error> {
                    let mut v = lexical(s.column(i))?;
                    for (r, x) in v.iter_mut().enumerate() {
                        if s.is_null(r) {
                            *x = None;
                        }
                    }
                    Ok(v)
                };
                attributes.insert("value".to_owned(), field(0)?);
                attributes.insert("lang".to_owned(), field(1)?);
            } else {
                attributes.insert("value".to_owned(), lexical(&values)?);
            }
            Ok(Rows {
                len: owners.len(),
                attributes,
            })
        }
        Source::Edge {
            edge,
            source,
            destination,
        } => {
            let src = subjects(store, root, info, tables, source).await?;
            let dst = subjects(store, root, info, tables, destination).await?;
            let ei = info
                .edges
                .iter()
                .find(|e| &format!("{}_{}_{}", e.src_type, e.edge_type, e.dst_type) == edge)
                .ok_or_else(|| Error::Invalid(format!("no edge type `{edge}`")))?;
            let adj = &ei.adj_lists[0];
            let parts = read_count(store, root, &paths::edge_vertex_count(ei, adj))
                .await?
                .div_ceil(ei.src_chunk_size);
            let (mut s_out, mut d_out) = (Vec::new(), Vec::new());
            for part in 0..parts {
                let n = read_count(store, root, &paths::edge_count(ei, adj, part)).await?;
                for chunk in 0..n.div_ceil(ei.chunk_size) {
                    for batch in read_adj_list_chunk(store, root, ei, adj, part, chunk).await? {
                        let ids = |name: &str| -> Result<Vec<i64>, Error> {
                            Ok(batch
                                .column_by_name(name)
                                .ok_or_else(|| missing(edge, name))?
                                .as_primitive::<Int64Type>()
                                .values()
                                .to_vec())
                        };
                        for (s, d) in ids(SRC_INDEX_COL)?.into_iter().zip(ids(DST_INDEX_COL)?) {
                            s_out.push(index(&src, s)?.clone());
                            d_out.push(index(&dst, d)?.clone());
                        }
                    }
                }
            }
            let len = s_out.len();
            attributes.insert("source".to_owned(), s_out);
            attributes.insert("target".to_owned(), d_out);
            Ok(Rows { len, attributes })
        }
    }
}

async fn cached<'a>(
    store: &dyn ObjectStore,
    root: &Path,
    info: &GraphInfo,
    tables: &'a mut HashMap<String, RecordBatch>,
    label: &str,
) -> Result<&'a RecordBatch, Error> {
    if !tables.contains_key(label) {
        let t = table(store, root, vertex_info(info, label)?).await?;
        tables.insert(label.to_owned(), t);
    }
    Ok(&tables[label])
}

/// A vertex type's subjects, by internal id.
async fn subjects(
    store: &dyn ObjectStore,
    root: &Path,
    info: &GraphInfo,
    tables: &mut HashMap<String, RecordBatch>,
    label: &str,
) -> Result<Vec<Option<String>>, Error> {
    let t = cached(store, root, info, tables, label).await?;
    lexical(
        t.column_by_name(SUBJECT)
            .ok_or_else(|| missing(label, SUBJECT))?,
    )
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

fn index(subjects: &[Option<String>], id: i64) -> Result<&Option<String>, Error> {
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

/// The lexical forms of a column's values, `None` for a null: what a term reads.
fn lexical(array: &ArrayRef) -> Result<Vec<Option<String>>, Error> {
    let each = |f: &dyn Fn(usize) -> String| {
        (0..array.len())
            .map(|i| array.is_valid(i).then(|| f(i)))
            .collect()
    };
    Ok(match array.data_type() {
        Arrow::Utf8 => each(&|i| array.as_string::<i32>().value(i).to_owned()),
        Arrow::Int64 => each(&|i| array.as_primitive::<Int64Type>().value(i).to_string()),
        Arrow::Int32 => each(&|i| array.as_primitive::<Int32Type>().value(i).to_string()),
        Arrow::Boolean => each(&|i| array.as_boolean().value(i).to_string()),
        Arrow::Date32 => each(&|i| civil(array.as_primitive::<Date32Type>().value(i))),
        other => return Err(Error::Invalid(format!("a column of {other}"))),
    })
}
