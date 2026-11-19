//! **`graphar-rdf`'s writer for JavaScript**: the module `@kanzo-tech/graphar/write` loads, and
//! nothing else. It takes a shape document and the triples as N-Triples, a batch at a time,
//! types each subject by the class its shape fixes ([`graphar_rdf::by_class`]), and writes the
//! graph through the host's `put`, a file at a time, as each chunk fills: memory is what
//! [`graphar_rdf::write`] says, not the graph.

use std::fmt;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt, TryStreamExt};
use graphar_rdf::shapes::{Shapes, shacl, shex};
use graphar_rdf::{Error, Options, by_class};
use js_sys::{AsyncIterator, Function, Promise, Uint8Array};
use object_store::path::Path;
use object_store::{
    CopyOptions, GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, ObjectStore,
    PutMultipartOptions, PutOptions, PutPayload, PutResult,
};
use oxrdf::Triple;
use oxttl::NTriplesParser;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use send_wrapper::SendWrapper;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

/// **Writes an RDF graph as GraphAr**, through `put(path, bytes)`, a host function answering a
/// promise, called once per file with its path relative to the graph's root.
///
/// - `syntax` and `shapes`: the shape document, `shacl` (Turtle), `shexc` or `shexj`.
/// - `triples`: an async iterator of strings, each whole N-Triples lines, the triples grouped by
///   subject (each subject's triples together).
/// - `name`: the graph's name, `<name>.graph.yml`; `chunk_size`: the rows of each chunk.
///
/// Answers `{ vertices: [[type, count]…], edges: [[edge, count]…], triples }`.
///
/// # Errors
/// What [`graphar_rdf::write`] refuses, the shape document's refusal, a batch that is not
/// N-Triples, and a rejected `put` or iterator, as an `Error` whose message says which.
#[wasm_bindgen(js_name = writeRdf)]
pub async fn write_rdf(
    put: Function,
    syntax: String,
    shapes: String,
    triples: AsyncIterator,
    name: String,
    chunk_size: f64,
) -> Result<JsValue, JsError> {
    let shapes = document(&syntax, &shapes)?;
    let store = Put(SendWrapper::new(put));
    let options = Options {
        name,
        chunk_size: chunk_size as u64,
        props: WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build(),
    };
    let subjects = by_class(shapes.schema(), lines(triples));
    let written = graphar_rdf::write(&store, &Path::from(""), &shapes, subjects, &options)
        .await
        .map_err(|e| JsError::new(&e.to_string()))?;
    let summary = serde_json::json!({
        "vertices": written.vertices,
        "edges": written.edges,
        "triples": written.triples,
    });
    js_sys::JSON::parse(&summary.to_string()).map_err(|_| JsError::new("the summary"))
}

/// The shape document of `syntax`.
fn document(syntax: &str, text: &str) -> Result<Shapes, JsError> {
    let shapes = match syntax {
        "shacl" => shacl::from_turtle(text),
        "shexc" => shex::from_compact(text),
        "shexj" => shex::from_json(text),
        other => return Err(JsError::new(&format!("`{other}` is no shape syntax"))),
    };
    shapes.map_err(|e| JsError::new(&e.to_string()))
}

/// The triples of an async iterator of N-Triples batches, one batch parsed at a time.
fn lines(
    iterator: AsyncIterator,
) -> impl futures_util::Stream<Item = Result<Triple, Error>> + Unpin {
    let batches = stream::try_unfold(iterator, |iterator| async move {
        let next = iterator.next().map_err(|e| Error::Syntax(message(&e)))?;
        let step = JsFuture::from(next)
            .await
            .map_err(|e| Error::Syntax(message(&e)))?;
        let done = js_sys::Reflect::get(&step, &"done".into()).is_ok_and(|d| d.is_truthy());
        if done {
            return Ok::<_, Error>(None);
        }
        let value = js_sys::Reflect::get(&step, &"value".into())
            .ok()
            .and_then(|v| v.as_string())
            .ok_or_else(|| Error::Syntax("a batch is not a string".to_owned()))?;
        let triples = NTriplesParser::new()
            .for_slice(value.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| Error::Syntax(e.to_string()))?;
        Ok(Some((stream::iter(triples.into_iter().map(Ok)), iterator)))
    });
    Box::pin(batches.try_flatten())
}

/// What a JavaScript error says.
fn message(e: &JsValue) -> String {
    e.dyn_ref::<js_sys::Error>()
        .map(|e| String::from(e.message()))
        .or_else(|| e.as_string())
        .unwrap_or_else(|| format!("{e:?}"))
}

/// The host's `put` as an [`ObjectStore`] that only puts: all the writer asks of one.
struct Put(SendWrapper<Function>);

impl fmt::Debug for Put {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Put")
    }
}

impl fmt::Display for Put {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the host's put")
    }
}

/// Every operation but `put`: the writer asks for none.
fn unsupported<T>() -> object_store::Result<T> {
    Err(object_store::Error::NotImplemented {
        operation: "anything but put".to_owned(),
        implementer: "the host's put".to_owned(),
    })
}

#[async_trait]
impl ObjectStore for Put {
    async fn put_opts(
        &self,
        location: &Path,
        payload: PutPayload,
        _: PutOptions,
    ) -> object_store::Result<PutResult> {
        let bytes: Vec<u8> = payload.iter().flat_map(|b| b.iter().copied()).collect();
        let call = || -> Result<Promise, JsValue> {
            let answer = self.0.call2(
                &JsValue::NULL,
                &location.as_ref().into(),
                &Uint8Array::from(bytes.as_slice()),
            )?;
            answer.dyn_into::<Promise>()
        };
        let fail = |e: JsValue| object_store::Error::Generic {
            store: "put",
            source: format!("{location}: {}", message(&e)).into(),
        };
        let promise = call().map_err(fail)?;
        SendWrapper::new(JsFuture::from(promise))
            .await
            .map_err(fail)?;
        Ok(PutResult {
            e_tag: None,
            version: None,
            extensions: Default::default(),
        })
    }

    async fn put_multipart_opts(
        &self,
        _: &Path,
        _: PutMultipartOptions,
    ) -> object_store::Result<Box<dyn MultipartUpload>> {
        unsupported()
    }

    async fn get_opts(&self, _: &Path, _: GetOptions) -> object_store::Result<GetResult> {
        unsupported()
    }

    fn delete_stream(
        &self,
        _: BoxStream<'static, object_store::Result<Path>>,
    ) -> BoxStream<'static, object_store::Result<Path>> {
        stream::once(async { unsupported() }).boxed()
    }

    fn list(&self, _: Option<&Path>) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
        stream::once(async { unsupported() }).boxed()
    }

    async fn list_with_delimiter(&self, _: Option<&Path>) -> object_store::Result<ListResult> {
        unsupported()
    }

    async fn copy_opts(&self, _: &Path, _: &Path, _: CopyOptions) -> object_store::Result<()> {
        unsupported()
    }
}
