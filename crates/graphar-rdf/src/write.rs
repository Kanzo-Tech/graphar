//! Triples and a schema to a GraphAr graph.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::pin::pin;
use std::sync::Arc;

use arrow_array::builder::ArrayBuilder;
use arrow_array::builder::{
    BooleanBuilder, Date32Builder, Int32Builder, Int64Builder, ListBuilder, StringBuilder,
    StructBuilder,
};
use arrow_array::{ArrayRef, Int64Array, RecordBatch};
use arrow_schema::{DataType as Arrow, Field};
use futures_util::{Stream, StreamExt, stream};
use graphar_info::{AdjListType, GraphInfo};
use graphar_parquet::{
    DST_INDEX_COL, Options as Writer, SRC_INDEX_COL, write_edge_vertex_count, write_edges,
    write_info, write_vertex_count, write_vertices,
};
use object_store::{ObjectStore, ObjectStoreExt, PutPayload, path::Path};
use oxrdf::{NamedOrBlankNode, Term, Triple};
use parquet::basic::Encoding;
use parquet::file::properties::WriterProperties;
use parquet::schema::types::ColumnPath;

use crate::schema::{Cardinality, Column, Error, Object, Schema, Storage, user_types};
use crate::term::{Value, parse};
use crate::typing::{Typing, assumed, validated};
use crate::{
    MAPPING_FILE, R2RML_FILE, RDF_LANG_STRING, RDF_TYPE, SHAPES_KEY, SUBJECT, plan, r2rml, rml,
};

/// How [`write()`] writes.
#[derive(Clone, Debug)]
pub struct Options {
    /// The graph's name: `<name>.graph.yml`.
    pub name: String,
    /// The rows of each chunk.
    pub chunk_size: u64,
    /// Parquet's writer settings: the codec.
    pub props: WriterProperties,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            name: "graph".to_owned(),
            chunk_size: 1 << 20,
            props: WriterProperties::default(),
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
    /// The distinct triples written.
    pub triples: u64,
}

/// A subject's place: its vertex type and row.
type Place = (usize, u64);

/// **`triples` grouped by subject**: each subject's triples together, the subjects in IRI order,
/// and a triple given twice given once. The stream [`write()`] takes, from triples held in memory
/// in any order (a parsed file, a test); a blank-node subject comes last, and [`write()`] refuses
/// it. In IRI order a graph is written exactly as it was before [`write()`] streamed.
pub fn grouped(
    triples: impl IntoIterator<Item = Triple>,
) -> impl Stream<Item = Result<Triple, Error>> + Unpin {
    let mut by_subject: BTreeMap<(bool, String), Vec<Triple>> = BTreeMap::new();
    for t in triples {
        let key = match &t.subject {
            NamedOrBlankNode::NamedNode(s) => (false, s.as_str().to_owned()),
            NamedOrBlankNode::BlankNode(b) => (true, b.as_str().to_owned()),
        };
        by_subject.entry(key).or_default().push(t);
    }
    stream::iter(by_subject.into_values().flat_map(distinct).map(Ok))
}

/// **Writes the triples of `triples` as the GraphAr graph `schema` reads**, under `root`: every
/// vertex table, every edge table, `mapping.ttl` and `mapping.r2rml.ttl`, the shape document, and
/// the information files last, so a reader that finds the graph document finds everything it
/// names.
///
/// `triples` is **grouped by subject**: all of a subject's triples are together, one subject after
/// another, in any order of subjects ([`grouped()`] groups triples held in memory). That is
/// checked, not trusted: a subject that reappears after another subject's triples is refused
/// ([`Error::Ungrouped`]). A source that fails ends the stream with its error
/// ([`Error::Syntax`]), and the graph is not committed.
///
/// Under [`Typing::Assume`] it **streams**. Each subject is typed from its own triples, placed and
/// appended to its type's open chunk, and a chunk is written as soon as it is full. What it holds
/// is what GraphAr's layout forces and no more:
///
/// - every subject seen, by its IRI, with its type and row: an adjacency list holds the row of an
///   edge's destination, and an edge may name a subject before or after its own triples;
/// - per vertex type, the rows of the open chunk, at most `chunk_size`, with their values;
/// - per edge type, the edges from a chunk of sources until that chunk's part is written: at the
///   chunk's end, or, if one of its edges names a subject not yet seen (a forward reference), when
///   the last such subject arrives. Such an edge holds its triple until then;
/// - one subject's triples.
///
/// So with every edge's destination seen before its source, memory is the subjects' IRIs plus a
/// chunk per type; each forward reference holds the part it is in until it is resolved, and at
/// worst — every part holding one until the end — the edges are held as pairs of rows. An edge
/// whose object never arrives as a subject of the destination's type is refused once the stream
/// ends.
///
/// Under [`Typing::Validate`] it does **not** stream: the validator takes the whole graph, so
/// every triple is held and every subject typed first, in IRI order, then placed as above.
///
/// **Rows are in arrival order.** A subject's row in its type, its internal id, is the order its
/// group arrived in among that type's subjects; before the writer streamed it was the order of
/// their IRIs. A stream in IRI order, which [`grouped()`] and [`Typing::Validate`] give, writes the
/// graph exactly as before; any other order writes the same graph up to the order of each type's
/// rows. Within a part the edges are sorted by source and then destination row.
///
/// Under [`Typing::Validate`] the shape document is saved exactly as it was read, as
/// [`crate::SHACL_FILE`], [`crate::SHEXC_FILE`] or [`crate::SHEXJ_FILE`] by its syntax, and the
/// graph document's `extra_info` names it under [`SHAPES_KEY`]: the labels of the types and
/// properties are the shapes', and a reader finds them there. Under [`Typing::Assume`] there is
/// no shape document to save, and none is written.
///
/// Only a graph that conforms to the closed reading of the schema is written, and every triple of
/// it has one place:
///
/// - every subject is an IRI that `typing` gives exactly one vertex type, and it is a row of that
///   type ([`Typing`]);
/// - every triple of it is the `rdf:type` triple its type's shape fixes (stored nowhere: the
///   mapping writes it), a value of a column of that type (an IRI, a tagged string, or a literal
///   of the column's datatype that is a value the column holds), or an edge of an edge type from
///   it, whose object is a row of the destination;
/// - a single-valued column has at most one value per subject.
///
/// A literal is stored as its value, and reads back in its canonical lexical form.
///
/// **A refused graph is not committed.** The information files are written last, and only once
/// every triple is placed, so on any refusal there is no graph document, and no reader finds a
/// graph. What a streaming write had written by then is left: the full chunks of each vertex type
/// and the parts of the edges from them, without count files, mappings, shape document or
/// information files. Nothing deletes them; a write to the same root overwrites them. Write into
/// an empty root: one that holds a graph keeps its information files, which a refused write
/// leaves naming chunks it has replaced.
///
/// # Errors
/// [`Error::Ungrouped`] for a subject that reappears; [`Error::Typing`] for the first subject, in
/// arrival order, that the typing gives no type or more than one (under [`Typing::Validate`], in
/// IRI order, before anything is written); [`Error::Unplaced`] for the first triple with no place;
/// what the schema's own check refuses; what the source, the validator, the store or the writers
/// refuse.
pub async fn write(
    store: &dyn ObjectStore,
    root: &Path,
    typing: &Typing<'_>,
    triples: impl Stream<Item = Result<Triple, Error>>,
    options: &Options,
) -> Result<Written, Error> {
    let schema = typing.schema();
    let plan = plan(schema)?;
    let text = rml(&plan);
    let mut info = schema.graph_info(&options.name, options.chunk_size);
    // The shape document validated against, saved as it was read, and named in the graph document.
    let shapes = match typing {
        Typing::Validate(shapes) => {
            let file = shapes.syntax().file();
            info.extra_info
                .push((SHAPES_KEY.to_owned(), file.to_owned()));
            Some((file, shapes.text()))
        }
        Typing::Assume(..) => None,
    };

    let mut groups = Groups {
        triples: pin!(triples),
        next: None,
    };
    let mut sink = Sink::new(store, root, schema, &info, options);
    match typing {
        Typing::Validate(shapes) => {
            let mut by_subject: BTreeMap<String, Vec<Triple>> = BTreeMap::new();
            while let Some((subject, ts)) = groups.next().await? {
                if by_subject.contains_key(&subject) {
                    return Err(Error::Ungrouped(subject));
                }
                by_subject.insert(subject, distinct(ts));
            }
            let typed = validated(shapes, &by_subject)?;
            for (subject, ts) in by_subject {
                let v = typed[&subject];
                sink.subject(subject, v, ts).await?;
            }
        }
        Typing::Assume(_, map) => {
            while let Some((subject, ts)) = groups.next().await? {
                if sink.places.contains_key(&subject) {
                    return Err(Error::Ungrouped(subject));
                }
                let ts = distinct(ts);
                let v = assumed(schema, map, &subject, &ts)?;
                sink.subject(subject, v, ts).await?;
            }
        }
    }
    let (vertices, edges, count, languages) = sink.finish().await?;

    put(store, root, MAPPING_FILE, text.into_bytes()).await?;
    put(
        store,
        root,
        R2RML_FILE,
        r2rml(&plan, &languages).into_bytes(),
    )
    .await?;
    if let Some((file, text)) = shapes {
        put(store, root, file, text.as_bytes().to_vec()).await?;
    }
    write_info(store, root, &info).await?;
    Ok(Written {
        info,
        vertices,
        edges,
        triples: count,
    })
}

/// A subject's triples once each, in the order of their N-Triples: the order a refusal names the
/// first of.
fn distinct(mut triples: Vec<Triple>) -> Vec<Triple> {
    triples.sort_by_cached_key(ToString::to_string);
    triples.dedup();
    triples
}

/// The triples of a stream, a subject's at a time: each run of triples with one subject. Whether a
/// subject runs twice is the reader's to check, against the subjects it already holds.
struct Groups<S> {
    triples: S,
    /// The first triple of the next run, read to find the end of this one.
    next: Option<Triple>,
}

impl<S: Stream<Item = Result<Triple, Error>> + Unpin> Groups<S> {
    async fn next(&mut self) -> Result<Option<(String, Vec<Triple>)>, Error> {
        let first = match self.next.take() {
            Some(t) => t,
            None => match self.triples.next().await {
                Some(t) => t?,
                None => return Ok(None),
            },
        };
        let subject = match &first.subject {
            NamedOrBlankNode::NamedNode(s) => s.as_str().to_owned(),
            NamedOrBlankNode::BlankNode(_) => {
                return Err(unplaced(&first, "its subject is a blank node"));
            }
        };
        let mut run = vec![first];
        while let Some(t) = self.triples.next().await {
            let t = t?;
            if matches!(&t.subject, NamedOrBlankNode::NamedNode(s) if s.as_str() == subject) {
                run.push(t);
            } else {
                self.next = Some(t);
                break;
            }
        }
        Ok(Some((subject, run)))
    }
}

/// An edge's destination: its row, or the triple naming a subject not yet seen.
enum Target {
    Row(u64),
    Ahead(Triple),
}

/// The edges of one edge type from one chunk of sources: one part of its adjacency list.
#[derive(Default)]
struct Part {
    /// The end of the sources' chunk, once it is written; `None` while it is open.
    end: Option<u64>,
    /// Each edge's source and destination rows.
    pairs: Vec<(u64, Target)>,
    /// How many of `pairs` are [`Target::Ahead`].
    ahead: usize,
}

/// A forward reference: slot `slot` of part `part` of edge type `edge`, the `arrival`-th edge.
struct Waiting {
    arrival: u64,
    edge: usize,
    part: u64,
    slot: usize,
}

/// A vertex type's open chunk.
struct Open {
    /// The type's rows so far, the open chunk's among them.
    count: u64,
    /// The open chunk's subjects.
    subjects: Vec<String>,
    /// The open chunk's values, column by column, row by row.
    cells: Vec<Vec<Vec<Value>>>,
}

/// The placement: subjects in, chunks and parts out as soon as they are complete.
struct Sink<'a> {
    store: &'a dyn ObjectStore,
    root: &'a Path,
    schema: &'a Schema,
    info: &'a GraphInfo,
    writer: Writer,
    chunk_size: u64,
    /// Every subject placed, by IRI.
    places: HashMap<String, Place>,
    /// Per vertex type.
    open: Vec<Open>,
    /// Per edge type: its parts not yet written, by index, and the edges written.
    parts: Vec<HashMap<u64, Part>>,
    edges: Vec<u64>,
    /// Per edge type, the index of its source and destination types.
    source: Vec<usize>,
    destination: Vec<usize>,
    /// The forward references, by the IRI they wait for.
    waiting: HashMap<String, Vec<Waiting>>,
    arrivals: u64,
    /// The languages of each language-tagged column, which R2RML writes a triples map apiece.
    languages: BTreeMap<String, BTreeSet<String>>,
    triples: u64,
}

impl<'a> Sink<'a> {
    fn new(
        store: &'a dyn ObjectStore,
        root: &'a Path,
        schema: &'a Schema,
        info: &'a GraphInfo,
        options: &Options,
    ) -> Self {
        let index = |label: &str| {
            schema
                .vertices
                .iter()
                .position(|v| v.label == label)
                .expect("checked")
        };
        Self {
            store,
            root,
            schema,
            info,
            writer: Writer {
                props: index_encoding(options.props.clone()),
                user_types: user_types(),
            },
            chunk_size: options.chunk_size,
            places: HashMap::new(),
            open: schema
                .vertices
                .iter()
                .map(|v| Open {
                    count: 0,
                    subjects: Vec::new(),
                    cells: vec![Vec::new(); v.columns.len()],
                })
                .collect(),
            parts: schema.edges.iter().map(|_| HashMap::new()).collect(),
            edges: vec![0; schema.edges.len()],
            source: schema.edges.iter().map(|e| index(&e.source)).collect(),
            destination: schema.edges.iter().map(|e| index(&e.destination)).collect(),
            waiting: HashMap::new(),
            arrivals: 0,
            languages: BTreeMap::new(),
            triples: 0,
        }
    }

    /// Places `subject`, a row of type `v`, with its triples (once each), and writes its type's
    /// chunk if that fills it, and any part the subject was the last forward reference of.
    async fn subject(&mut self, subject: String, v: usize, ts: Vec<Triple>) -> Result<(), Error> {
        let schema = self.schema;
        let vertex = &schema.vertices[v];
        let row = self.open[v].count;
        let chunk = row / self.chunk_size;
        self.places.insert(subject.clone(), (v, row));

        // The edges that named it before it arrived.
        for w in self.waiting.remove(&subject).unwrap_or_default() {
            let part = self.parts[w.edge].get_mut(&w.part).expect("a waiting part");
            if self.destination[w.edge] != v {
                let Target::Ahead(t) = &part.pairs[w.slot].1 else {
                    unreachable!("a waiting edge")
                };
                return Err(not_a_row(t, &schema.edges[w.edge].destination));
            }
            part.pairs[w.slot].1 = Target::Row(row);
            part.ahead -= 1;
            if part.ahead == 0 && part.end.is_some() {
                self.flush(w.edge, w.part).await?;
            }
        }

        let mut cells: Vec<Vec<Value>> = vec![Vec::new(); vertex.columns.len()];
        for t in &ts {
            let p = t.predicate.as_str();
            if p == RDF_TYPE
                && vertex
                    .class
                    .as_deref()
                    .is_some_and(|c| matches!(&t.object, Term::NamedNode(o) if o.as_str() == c))
            {
                continue;
            }
            if let Some(c) = vertex.columns.iter().position(|c| c.predicate == p) {
                let column = &vertex.columns[c];
                let value = value(column, &t.object).ok_or_else(|| {
                    unplaced(
                        t,
                        &format!("not a value of {}.{}", vertex.label, column.name),
                    )
                })?;
                let cell = &mut cells[c];
                if !cell.contains(&value) {
                    if column.cardinality == Cardinality::Single && !cell.is_empty() {
                        return Err(unplaced(
                            t,
                            &format!("{}.{} has at most one value", vertex.label, column.name),
                        ));
                    }
                    cell.push(value);
                }
                continue;
            }
            let edge = schema
                .edges
                .iter()
                .position(|e| e.source == vertex.label && e.predicate == p)
                .ok_or_else(|| {
                    unplaced(t, &format!("{} has no column or edge of it", vertex.label))
                })?;
            let destination = &schema.edges[edge].destination;
            let target = match &t.object {
                Term::NamedNode(o) => match self.places.get(o.as_str()) {
                    Some(&(d, r)) if d == self.destination[edge] => Target::Row(r),
                    Some(_) => return Err(not_a_row(t, destination)),
                    None => Target::Ahead(t.clone()),
                },
                _ => return Err(not_a_row(t, destination)),
            };
            let part = self.parts[edge].entry(chunk).or_default();
            if let Target::Ahead(_) = target {
                let Term::NamedNode(o) = &t.object else {
                    unreachable!("an IRI")
                };
                self.waiting
                    .entry(o.as_str().to_owned())
                    .or_default()
                    .push(Waiting {
                        arrival: self.arrivals,
                        edge,
                        part: chunk,
                        slot: part.pairs.len(),
                    });
                self.arrivals += 1;
                part.ahead += 1;
            }
            part.pairs.push((row, target));
        }
        self.triples += ts.len() as u64;

        let open = &mut self.open[v];
        for (c, (column, mut cell)) in vertex.columns.iter().zip(cells).enumerate() {
            cell.sort_by_key(crate::term::lexical_of);
            if column.storage() == Storage::LangString {
                let tags = cell.iter().filter_map(|value| match value {
                    Value::Text(t) => t.split_once('@').map(|(tag, _)| tag.to_owned()),
                    _ => None,
                });
                self.languages
                    .entry(format!("{}/{}", vertex.label, column.name))
                    .or_default()
                    .extend(tags);
            }
            open.cells[c].push(cell);
        }
        open.subjects.push(subject);
        open.count += 1;
        if open.count.is_multiple_of(self.chunk_size) {
            self.close(v).await?;
        }
        Ok(())
    }

    /// Writes the open chunk of type `v`, and the part of every edge type from it whose
    /// destinations are all known.
    async fn close(&mut self, v: usize) -> Result<(), Error> {
        let schema = self.schema;
        let vertex = &schema.vertices[v];
        let open = &mut self.open[v];
        let chunk = (open.count - 1) / self.chunk_size;
        let mut columns: Vec<(String, ArrayRef)> =
            vec![(SUBJECT.to_owned(), strings(&open.subjects))];
        for (column, cells) in vertex.columns.iter().zip(&open.cells) {
            columns.push((column.name.clone(), array(column, cells)?));
        }
        let batch = RecordBatch::try_from_iter(columns)?;
        write_vertices(
            self.store,
            self.root,
            &self.info.vertices[v],
            chunk,
            &[batch],
            &self.writer,
        )
        .await?;
        open.subjects.clear();
        open.cells.iter_mut().for_each(Vec::clear);
        let end = open.count;
        for e in 0..schema.edges.len() {
            if self.source[e] != v {
                continue;
            }
            let part = self.parts[e].entry(chunk).or_default();
            part.end = Some(end);
            if part.ahead == 0 {
                self.flush(e, chunk).await?;
            }
        }
        Ok(())
    }

    /// Writes part `p` of edge type `e`, whose sources' chunk is written and whose destinations
    /// are all known.
    async fn flush(&mut self, e: usize, p: u64) -> Result<(), Error> {
        let part = self.parts[e].remove(&p).expect("a part");
        let mut pairs: Vec<(i64, i64)> = part
            .pairs
            .into_iter()
            .map(|(src, dst)| match dst {
                Target::Row(dst) => (src as i64, dst as i64),
                Target::Ahead(_) => unreachable!("every destination known"),
            })
            .collect();
        pairs.sort_unstable();
        let src: ArrayRef = Arc::new(Int64Array::from_iter_values(pairs.iter().map(|p| p.0)));
        let dst: ArrayRef = Arc::new(Int64Array::from_iter_values(pairs.iter().map(|p| p.1)));
        let batch = RecordBatch::try_from_iter([(SRC_INDEX_COL, src), (DST_INDEX_COL, dst)])?;
        let end = part.end.expect("a written chunk");
        self.edges[e] += write_edges(
            self.store,
            self.root,
            &self.info.edges[e],
            AdjListType::OrderedBySource,
            p * self.chunk_size..end,
            &[batch],
            &self.writer,
        )
        .await?;
        Ok(())
    }

    /// Refuses an edge to a subject that never arrived, then writes every open chunk, the parts
    /// from them, and the count files: the rows of each vertex type, the edges of each edge type,
    /// the triples, and the languages of each tagged column.
    #[allow(clippy::type_complexity)]
    async fn finish(
        mut self,
    ) -> Result<
        (
            Vec<(String, u64)>,
            Vec<(String, u64)>,
            u64,
            BTreeMap<String, BTreeSet<String>>,
        ),
        Error,
    > {
        let schema = self.schema;
        if let Some(w) = self.waiting.values().flatten().min_by_key(|w| w.arrival) {
            let Target::Ahead(t) = &self.parts[w.edge][&w.part].pairs[w.slot].1 else {
                unreachable!("a waiting edge")
            };
            return Err(not_a_row(t, &schema.edges[w.edge].destination));
        }
        for v in 0..schema.vertices.len() {
            if !self.open[v].count.is_multiple_of(self.chunk_size) {
                self.close(v).await?;
            }
        }
        debug_assert!(self.parts.iter().all(HashMap::is_empty));
        let mut vertices = Vec::new();
        for (v, vertex) in schema.vertices.iter().enumerate() {
            let n = self.open[v].count;
            write_vertex_count(self.store, self.root, &self.info.vertices[v], n).await?;
            vertices.push((vertex.label.clone(), n));
        }
        let mut edges = Vec::new();
        for (e, edge) in schema.edges.iter().enumerate() {
            write_edge_vertex_count(
                self.store,
                self.root,
                &self.info.edges[e],
                AdjListType::OrderedBySource,
                self.open[self.source[e]].count,
            )
            .await?;
            edges.push((edge.triple(), self.edges[e]));
        }
        Ok((vertices, edges, self.triples, self.languages))
    }
}

fn not_a_row(t: &Triple, destination: &str) -> Error {
    unplaced(t, &format!("its object is not a row of {destination}"))
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

fn unplaced(t: &Triple, reason: &str) -> Error {
    Error::Unplaced {
        triple: format!("{t} ."),
        reason: reason.to_owned(),
    }
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
