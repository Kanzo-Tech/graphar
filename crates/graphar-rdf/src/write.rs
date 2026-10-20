//! Triples and a schema to a GraphAr graph.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use arrow_array::builder::ArrayBuilder;
use arrow_array::builder::{
    BooleanBuilder, Date32Builder, Int32Builder, Int64Builder, ListBuilder, StringBuilder,
    StructBuilder,
};
use arrow_array::{ArrayRef, Int64Array, RecordBatch};
use arrow_schema::{DataType as Arrow, Field};
use graphar_info::{AdjListType, GraphInfo};
use graphar_parquet::{
    DST_INDEX_COL, Options as Writer, SRC_INDEX_COL, write_edges, write_info, write_vertices,
};
use object_store::{ObjectStore, ObjectStoreExt, PutPayload, path::Path};
use oxrdf::{NamedOrBlankNode, Term, Triple};
use parquet::basic::Encoding;
use parquet::file::properties::WriterProperties;
use parquet::schema::types::ColumnPath;

use crate::schema::{
    Cardinality, Column, Error, Object, RESIDUAL_COLUMNS, Schema, Storage, user_types,
};
use crate::term::{Value, parse};
use crate::{MAPPING_FILE, RDF_LANG_STRING, RDF_TYPE, SUBJECT, mapping};

/// How [`write()`] writes.
#[derive(Clone, Debug)]
pub struct Options {
    /// The graph's name: `<name>.graph.yml`.
    pub name: String,
    /// The rows of each chunk.
    pub chunk_size: u64,
    /// Parquet's writer settings: the codec, the row groups.
    pub props: WriterProperties,
    /// Whether to count what is written (`graphar-stats`): the footer of every vertex chunk, and a
    /// Puffin file for each vertex type of more than one chunk.
    pub stats: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            name: "graph".to_owned(),
            chunk_size: 1 << 20,
            props: WriterProperties::default(),
            stats: true,
        }
    }
}

/// What [`write()`] wrote.
#[derive(Clone, Debug)]
pub struct Written {
    /// The information files.
    pub info: GraphInfo,
    /// The rows of each vertex type, in the schema's order.
    pub vertices: Vec<(String, u64)>,
    /// The edges of each edge type, in the schema's order.
    pub edges: Vec<(String, u64)>,
    /// The triples in the residual table.
    pub residual: u64,
    /// The distinct triples written.
    pub triples: u64,
}

/// A subject's place: its vertex type and row.
type Place = (usize, usize);

/// **Writes `triples` as the GraphAr graph `schema` reads**, under `root`: every vertex table,
/// every edge table, the residual table, a Puffin file per vertex type of several chunks,
/// `mapping.ttl`, and the
/// information files last, so a reader that finds the graph document finds everything it names.
///
/// A subject is a row of the first vertex type whose class it is an instance of, by an
/// `rdf:type` triple; a blank node is a row of none. Of its triples, the one that types it goes
/// into the row, as does each value of a column whose lexical form the column gives back, one
/// per subject for a single-valued column, and each object that is a row of an edge type's
/// destination. Everything else, a duplicate excepted, is a row of the residual table.
///
/// # Errors
/// What [`Schema::check`] refuses, and what the store or the writers refuse.
pub async fn write(
    store: &dyn ObjectStore,
    root: &Path,
    schema: &Schema,
    triples: impl IntoIterator<Item = Triple>,
    options: &Options,
) -> Result<Written, Error> {
    let text = mapping(schema)?;
    let info = schema.graph_info(&options.name, options.chunk_size);

    let distinct: HashSet<Triple> = triples.into_iter().collect();
    let count = distinct.len() as u64;
    let mut by_subject: HashMap<NamedOrBlankNode, Vec<Triple>> = HashMap::new();
    for t in distinct {
        by_subject.entry(t.subject.clone()).or_default().push(t);
    }

    // Each typed subject's place, the rows of a type sorted by IRI.
    let class_of: HashMap<&str, usize> = schema
        .vertices
        .iter()
        .enumerate()
        .filter_map(|(i, v)| Some((v.class.as_deref()?, i)))
        .collect();
    let mut rows: Vec<BTreeSet<String>> = vec![BTreeSet::new(); schema.vertices.len()];
    for (subject, ts) in &by_subject {
        let NamedOrBlankNode::NamedNode(s) = subject else {
            continue;
        };
        let typed = ts
            .iter()
            .filter(|t| t.predicate.as_str() == RDF_TYPE)
            .filter_map(|t| match &t.object {
                Term::NamedNode(c) => class_of.get(c.as_str()).copied(),
                _ => None,
            })
            .min();
        if let Some(v) = typed {
            rows[v].insert(s.as_str().to_owned());
        }
    }
    let rows: Vec<Vec<String>> = rows.into_iter().map(|r| r.into_iter().collect()).collect();
    let place: HashMap<&str, Place> = rows
        .iter()
        .enumerate()
        .flat_map(|(v, r)| r.iter().enumerate().map(move |(i, s)| (s.as_str(), (v, i))))
        .collect();

    // Each column's values, row by row; each edge type's pairs; the rest.
    let mut cells: Vec<Vec<Vec<Vec<Value>>>> = schema
        .vertices
        .iter()
        .zip(&rows)
        .map(|(v, r)| vec![vec![Vec::new(); r.len()]; v.columns.len()])
        .collect();
    let mut pairs: Vec<Vec<(i64, i64)>> = vec![Vec::new(); schema.edges.len()];
    let mut residual: Vec<[Option<String>; 5]> = Vec::new();
    for (subject, ts) in by_subject {
        let here = match &subject {
            NamedOrBlankNode::NamedNode(s) => place.get(s.as_str()).copied(),
            NamedOrBlankNode::BlankNode(_) => None,
        };
        let Some((v, row)) = here else {
            residual.extend(ts.iter().map(row_of));
            continue;
        };
        let vertex = &schema.vertices[v];
        let class = vertex.class.as_deref();
        // By predicate, the triples of this subject not yet placed.
        let mut rest: Vec<Option<Triple>> = ts.into_iter().map(Some).collect();
        for slot in &mut rest {
            let typing = slot.as_ref().is_some_and(|t| {
                t.predicate.as_str() == RDF_TYPE
                    && matches!(&t.object, Term::NamedNode(c) if Some(c.as_str()) == class)
            });
            if typing {
                *slot = None;
            }
        }
        for (c, column) in vertex.columns.iter().enumerate() {
            let fits: Vec<(usize, Value)> = rest
                .iter()
                .enumerate()
                .filter_map(|(i, t)| {
                    let t = t.as_ref()?;
                    (t.predicate.as_str() == column.predicate)
                        .then(|| value(column, &t.object))
                        .flatten()
                        .map(|value| (i, value))
                })
                .collect();
            if column.cardinality == Cardinality::Single && fits.len() != 1 {
                continue;
            }
            let mut values = Vec::with_capacity(fits.len());
            for (i, value) in fits {
                rest[i] = None;
                values.push(value);
            }
            values.sort_by_key(crate::term::lexical_of);
            cells[v][c][row] = values;
        }
        for (e, edge) in schema.edges.iter().enumerate() {
            if edge.source != vertex.label {
                continue;
            }
            let destination = schema
                .vertices
                .iter()
                .position(|d| d.label == edge.destination)
                .expect("checked");
            for slot in &mut rest {
                let Some(t) = slot.as_ref() else { continue };
                if t.predicate.as_str() != edge.predicate {
                    continue;
                }
                let Term::NamedNode(o) = &t.object else {
                    continue;
                };
                if let Some(&(d, target)) = place.get(o.as_str())
                    && d == destination
                {
                    pairs[e].push((row as i64, target as i64));
                    *slot = None;
                }
            }
        }
        residual.extend(rest.iter().flatten().map(row_of));
    }

    let writer = Writer {
        props: index_encoding(options.props.clone()),
        user_types: user_types(),
        footer: options.stats.then(graphar_stats::footer),
    };
    let mut vertices = Vec::new();
    for (v, vertex) in schema.vertices.iter().enumerate() {
        let mut columns: Vec<(String, ArrayRef)> = vec![(SUBJECT.to_owned(), strings(&rows[v]))];
        for (c, column) in vertex.columns.iter().enumerate() {
            columns.push((column.name.clone(), array(column, &cells[v][c])?));
        }
        let batch = RecordBatch::try_from_iter(columns)?;
        let n = write_table(store, root, &info.vertices[v], &batch, &writer, options).await?;
        vertices.push((vertex.label.clone(), n));
    }
    residual.sort();
    let batch =
        RecordBatch::try_from_iter(RESIDUAL_COLUMNS.iter().enumerate().map(|(i, (n, _))| {
            let column: ArrayRef = Arc::new(
                residual
                    .iter()
                    .map(|r| r[i].as_deref())
                    .collect::<arrow_array::StringArray>(),
            );
            ((*n).to_owned(), column)
        }))?;
    let residual_info = &info.vertices[schema.vertices.len()];
    let residual = write_table(store, root, residual_info, &batch, &writer, options).await?;

    let mut edges = Vec::new();
    for (e, edge) in schema.edges.iter().enumerate() {
        let mut ps = std::mem::take(&mut pairs[e]);
        ps.sort_unstable();
        let src: ArrayRef = Arc::new(Int64Array::from_iter_values(ps.iter().map(|p| p.0)));
        let dst: ArrayRef = Arc::new(Int64Array::from_iter_values(ps.iter().map(|p| p.1)));
        let batch = RecordBatch::try_from_iter([(SRC_INDEX_COL, src), (DST_INDEX_COL, dst)])?;
        let source = schema
            .vertices
            .iter()
            .position(|v| v.label == edge.source)
            .expect("checked");
        let n = write_edges(
            store,
            root,
            &info.edges[e],
            AdjListType::OrderedBySource,
            rows[source].len() as u64,
            &[batch],
            &writer,
        )
        .await?;
        edges.push((edge.triple(), n));
    }

    put(store, root, MAPPING_FILE, text.into_bytes()).await?;
    write_info(store, root, &info).await?;
    Ok(Written {
        info,
        vertices,
        edges,
        residual,
        triples: count,
    })
}

/// `props`, with GraphAr's internal-id columns delta-encoded and out of the dictionary: a vertex
/// chunk's ids are consecutive, an adjacency list's sources sorted, so their deltas are small, and a
/// dictionary of distinct ids is as large as the ids. A caller's own setting for these columns is
/// replaced; every other setting is the caller's.
fn index_encoding(props: WriterProperties) -> WriterProperties {
    let mut builder = props.into_builder();
    for column in [
        graphar_parquet::VERTEX_INDEX_COL,
        SRC_INDEX_COL,
        DST_INDEX_COL,
    ] {
        let path = ColumnPath::from(column);
        builder = builder
            .set_column_dictionary_enabled(path.clone(), false)
            .set_column_encoding(path, Encoding::DELTA_BINARY_PACKED);
    }
    builder.build()
}

async fn write_table(
    store: &dyn ObjectStore,
    root: &Path,
    info: &graphar_info::VertexInfo,
    batch: &RecordBatch,
    writer: &Writer,
    options: &Options,
) -> Result<u64, Error> {
    let n = write_vertices(store, root, info, std::slice::from_ref(batch), writer).await?;
    // One chunk's footer already holds the table's exact counts; a sketch is for the distinct
    // count of several, which their footers cannot give.
    if options.stats && n > info.chunk_size {
        let puffin = graphar_stats::puffin(info, batch)?;
        put(store, root, &graphar_stats::puffin_path(info), puffin).await?;
    }
    Ok(n)
}

async fn put(
    store: &dyn ObjectStore,
    root: &Path,
    relative: &str,
    bytes: Vec<u8>,
) -> Result<(), Error> {
    let path: Path = root
        .parts()
        .chain(
            Path::parse(relative)
                .map_err(|e| Error::Invalid(e.to_string()))?
                .parts(),
        )
        .collect();
    store.put(&path, PutPayload::from(bytes)).await?;
    Ok(())
}

/// The value an object is in `column`, if it is one the column holds and gives back.
fn value(column: &Column, object: &Term) -> Option<Value> {
    match (&column.object, object) {
        (Object::Iri, Term::NamedNode(n)) => Some(Value::Text(n.as_str().to_owned())),
        (Object::Literal(dt), Term::Literal(l))
            if l.language().is_none() && l.datatype().as_str() == dt =>
        {
            parse(column.storage(), l.value())
        }
        (Object::LangString, Term::Literal(l)) => l
            .language()
            .map(|tag| Value::Text(format!("{tag}@{}", l.value()))),
        (Object::Literal(dt), Term::Literal(l)) if dt == RDF_LANG_STRING => l
            .language()
            .map(|tag| Value::Text(format!("{tag}@{}", l.value()))),
        _ => None,
    }
}

/// The residual row of a triple.
fn row_of(t: &Triple) -> [Option<String>; 5] {
    let node = |n: &NamedOrBlankNode| match n {
        NamedOrBlankNode::NamedNode(n) => n.as_str().to_owned(),
        NamedOrBlankNode::BlankNode(b) => format!("_:{}", b.as_str()),
    };
    let (object, datatype, lang) = match &t.object {
        Term::NamedNode(n) => (n.as_str().to_owned(), None, None),
        Term::BlankNode(b) => (format!("_:{}", b.as_str()), None, None),
        Term::Literal(l) => (
            l.value().to_owned(),
            Some(l.datatype().as_str().to_owned()),
            l.language().map(str::to_owned),
        ),
    };
    [
        Some(node(&t.subject)),
        Some(t.predicate.as_str().to_owned()),
        Some(object),
        datatype,
        lang,
    ]
}

fn strings(values: &[String]) -> ArrayRef {
    Arc::new(arrow_array::StringArray::from_iter_values(values))
}

/// A column of `cells`, one entry per row: no value is a null, and a set is a list.
fn array(column: &Column, cells: &[Vec<Value>]) -> Result<ArrayRef, Error> {
    let storage = column.storage();
    if column.cardinality == Cardinality::Single {
        let mut b = builder(storage);
        for cell in cells {
            b.append(cell.first());
        }
        return Ok(b.finish());
    }
    let mut lists = ListBuilder::new(builder(storage)).with_field(Arc::new(Field::new(
        "item",
        arrow_type(storage),
        false,
    )));
    for cell in cells {
        for v in cell {
            lists.values().append(Some(v));
        }
        lists.append(true);
    }
    Ok(Arc::new(lists.finish()))
}

fn arrow_type(storage: Storage) -> Arrow {
    match storage {
        Storage::String => Arrow::Utf8,
        Storage::Int64 => Arrow::Int64,
        Storage::Int32 => Arrow::Int32,
        Storage::Bool => Arrow::Boolean,
        Storage::Date => Arrow::Date32,
        Storage::LangString => crate::schema::lang_string_type(),
    }
}

/// An Arrow builder of one storage.
enum Builder {
    String(StringBuilder),
    Int64(Int64Builder),
    Int32(Int32Builder),
    Bool(BooleanBuilder),
    Date(Date32Builder),
    Lang(StructBuilder),
}

fn builder(storage: Storage) -> Builder {
    match storage {
        Storage::String => Builder::String(StringBuilder::new()),
        Storage::Int64 => Builder::Int64(Int64Builder::new()),
        Storage::Int32 => Builder::Int32(Int32Builder::new()),
        Storage::Bool => Builder::Bool(BooleanBuilder::new()),
        Storage::Date => Builder::Date(Date32Builder::new()),
        Storage::LangString => {
            let Arrow::Struct(fields) = crate::schema::lang_string_type() else {
                unreachable!("a struct")
            };
            Builder::Lang(StructBuilder::from_fields(fields, 0))
        }
    }
}

impl ArrayBuilder for Builder {
    fn len(&self) -> usize {
        self.inner().len()
    }
    fn finish(&mut self) -> ArrayRef {
        self.inner_mut().finish()
    }
    fn finish_cloned(&self) -> ArrayRef {
        self.inner().finish_cloned()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn into_box_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}

impl Builder {
    fn inner(&self) -> &dyn arrow_array::builder::ArrayBuilder {
        match self {
            Self::String(b) => b,
            Self::Int64(b) => b,
            Self::Int32(b) => b,
            Self::Bool(b) => b,
            Self::Date(b) => b,
            Self::Lang(b) => b,
        }
    }
    fn inner_mut(&mut self) -> &mut dyn arrow_array::builder::ArrayBuilder {
        match self {
            Self::String(b) => b,
            Self::Int64(b) => b,
            Self::Int32(b) => b,
            Self::Bool(b) => b,
            Self::Date(b) => b,
            Self::Lang(b) => b,
        }
    }

    fn append(&mut self, value: Option<&Value>) {
        match (self, value) {
            (Self::String(b), Some(Value::Text(t))) => b.append_value(t),
            (Self::String(b), _) => b.append_null(),
            (Self::Int64(b), Some(Value::Int64(i))) => b.append_value(*i),
            (Self::Int64(b), _) => b.append_null(),
            (Self::Int32(b), Some(Value::Int32(i))) => b.append_value(*i),
            (Self::Int32(b), _) => b.append_null(),
            (Self::Bool(b), Some(Value::Bool(v))) => b.append_value(*v),
            (Self::Bool(b), _) => b.append_null(),
            (Self::Date(b), Some(Value::Date(d))) => b.append_value(*d),
            (Self::Date(b), _) => b.append_null(),
            (Self::Lang(b), value) => {
                // A tagged value is held as `tag@form` until here: the tag has no `@`.
                let (tag, form) = match value {
                    Some(Value::Text(t)) => t.split_once('@').map_or(("", ""), |(t, f)| (t, f)),
                    _ => ("", ""),
                };
                b.field_builder::<StringBuilder>(0)
                    .expect("value")
                    .append_value(form);
                b.field_builder::<StringBuilder>(1)
                    .expect("lang")
                    .append_value(tag);
                b.append(value.is_some());
            }
        }
    }
}
