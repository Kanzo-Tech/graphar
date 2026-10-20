//! Zarr, the layout of the earlier Zarr paper as ZarrDF (github.com/weso/ZarrDF) writes it: three
//! dictionaries of terms in the root group's attributes, and three `uint64` arrays of shape
//! `[triples, 2]`, `subject`, `predicate` and `object`, each sorted by its component and chunked
//! by a `rectangular` grid so that chunk `i` holds the other two ids of every triple whose
//! component is term `i`. A pattern with a bound term reads one chunk of one array. Codec gzip,
//! level 5.
//!
//! Written here and not by ZarrDF: at its only commit (`f4652c4`) it chunks the triples in input
//! order rather than by id, stores the subject and predicate ids in every array, and offsets the
//! rows of any term past the 10,000th, so a lookup returns another term's triples or none. This is
//! the layout it describes, with those three corrected.

use std::path::Path;
use std::sync::Arc;

use oxrdf::{NamedOrBlankNode, Term, Triple};
use serde_json::Value;
use zarrs::array::chunk_grid::{
    RectangularChunkGridConfiguration, RectangularChunkGridDimensionConfiguration,
};
use zarrs::array::{Array, ArrayBuilder, DataType, codec};
use zarrs::filesystem::FilesystemStore;
use zarrs::group::{Group, GroupBuilder};
use zarrs::metadata::v3::MetadataV3;
use zarrs::storage::ReadableStorageTraits;

/// The three arrays, by the component they are sorted on.
pub const ARRAYS: [&str; 3] = ["subject", "predicate", "object"];

/// A term as ZarrDF writes it: an IRI in angle brackets, a blank node `_:` and its label, a simple
/// literal bare, any other quoted with its datatype or tag.
pub fn term(t: &Term) -> String {
    match t {
        Term::NamedNode(n) => n.to_string(),
        Term::BlankNode(b) => format!("_:{}", b.as_str()),
        Term::Literal(l) => match (l.language(), l.datatype().as_str()) {
            (Some(lang), _) => format!("\"{}\"@{lang}", l.value()),
            (None, "http://www.w3.org/2001/XMLSchema#string") => l.value().to_owned(),
            (None, dt) => format!("\"{}\"^^<{dt}>", l.value()),
        },
    }
}

fn subject(s: &NamedOrBlankNode) -> String {
    term(&Term::from(s.clone()))
}

/// Writes `triples` under `dir`.
pub fn write(dir: &Path, triples: &[Triple]) {
    let _ = std::fs::remove_dir_all(dir);
    let store = Arc::new(FilesystemStore::new(dir).expect("a Zarr store"));
    let rows: Vec<[String; 3]> = triples
        .iter()
        .map(|t| {
            [
                subject(&t.subject),
                t.predicate.to_string(),
                term(&t.object),
            ]
        })
        .collect();
    let dictionaries: Vec<Vec<String>> = (0..3)
        .map(|k| {
            let mut d: Vec<String> = rows.iter().map(|r| r[k].clone()).collect();
            d.sort_unstable();
            d.dedup();
            d
        })
        .collect();
    let ids: Vec<[u64; 3]> = rows
        .iter()
        .map(|r| {
            std::array::from_fn(|k| {
                dictionaries[k]
                    .binary_search(&r[k])
                    .expect("in the dictionary") as u64
            })
        })
        .collect();

    let mut group = GroupBuilder::new()
        .build(store.clone(), "/")
        .expect("the group");
    for (k, d) in dictionaries.iter().enumerate() {
        group
            .attributes_mut()
            .insert(ARRAYS[k].to_owned(), Value::from(d.clone()));
    }
    group.store_metadata().expect("the group metadata");

    for k in 0..3 {
        let others: [usize; 2] = match k {
            0 => [1, 2],
            1 => [0, 2],
            _ => [0, 1],
        };
        let mut sorted = ids.clone();
        sorted.sort_unstable_by_key(|t| (t[k], t[others[0]], t[others[1]]));
        let mut sizes: Vec<std::num::NonZeroU64> = Vec::new();
        let mut starts = Vec::new();
        for (i, t) in sorted.iter().enumerate() {
            if i == 0 || sorted[i - 1][k] != t[k] {
                starts.push(i);
                sizes.push(std::num::NonZeroU64::MIN);
            } else {
                let last = sizes.last_mut().expect("a chunk");
                *last = last.checked_add(1).expect("no overflow");
            }
        }
        let grid = MetadataV3::new_with_configuration(
            "rectangular",
            RectangularChunkGridConfiguration {
                chunk_shape: vec![
                    RectangularChunkGridDimensionConfiguration::Varying(sizes.clone().into()),
                    RectangularChunkGridDimensionConfiguration::Fixed(
                        std::num::NonZeroU64::new(2).expect("two"),
                    ),
                ],
            },
        );
        let array = ArrayBuilder::new(vec![sorted.len() as u64, 2], grid, DataType::UInt64, 0u64)
            .bytes_to_bytes_codecs(vec![Arc::new(
                codec::GzipCodec::new(5).expect("gzip level 5"),
            )])
            .dimension_names(Some(others.map(|o| ARRAYS[o])))
            .build(store.clone(), &format!("/{}", ARRAYS[k]))
            .expect("the array");
        array.store_metadata().expect("the array metadata");
        for (chunk, (&start, size)) in starts.iter().zip(&sizes).enumerate() {
            let elements: Vec<u64> = sorted[start..start + size.get() as usize]
                .iter()
                .flat_map(|t| [t[others[0]], t[others[1]]])
                .collect();
            array
                .store_chunk_elements(&[chunk as u64, 0], &elements)
                .expect("a chunk");
        }
    }
}

/// A triple pattern: each position a term as [`term`] writes it, or open; and a test on the
/// object's text.
pub struct Pattern<'a> {
    pub terms: [Option<String>; 3],
    pub object_filter: Option<&'a dyn Fn(&str) -> bool>,
}

/// The array a pattern reads: that of its first bound position.
fn plan(pattern: &Pattern<'_>) -> usize {
    pattern
        .terms
        .iter()
        .position(Option::is_some)
        .expect("a bound position")
}

/// The ids of the pattern's terms, `None` for an open position; `Err` if a bound term is in no
/// dictionary, which no triple matches.
fn bind(dictionaries: &[Vec<String>], pattern: &Pattern<'_>) -> Result<[Option<u64>; 3], ()> {
    let mut out = [None; 3];
    for k in 0..3 {
        if let Some(t) = &pattern.terms[k] {
            out[k] = Some(dictionaries[k].binary_search(t).map_err(|_| ())? as u64);
        }
    }
    Ok(out)
}

/// The triples of chunk `id` of array `k` that match.
fn count(
    k: usize,
    id: u64,
    pairs: &[u64],
    bound: [Option<u64>; 3],
    objects: &[String],
    pattern: &Pattern<'_>,
) -> u64 {
    pairs
        .chunks_exact(2)
        .map(|p| match k {
            0 => [id, p[0], p[1]],
            1 => [p[0], id, p[1]],
            _ => [p[0], p[1], id],
        })
        .filter(|t| (0..3).all(|c| bound[c].is_none_or(|b| b == t[c])))
        .filter(|t| {
            pattern
                .object_filter
                .is_none_or(|f| f(&objects[t[2] as usize]))
        })
        .count() as u64
}

fn dictionaries(attributes: &serde_json::Map<String, Value>) -> Vec<Vec<String>> {
    ARRAYS
        .iter()
        .map(|a| {
            attributes[*a]
                .as_array()
                .expect("a dictionary")
                .iter()
                .map(|t| t.as_str().expect("a term").to_owned())
                .collect()
        })
        .collect()
}

/// A Zarr graph open on a store.
pub struct Zarr<S: ?Sized> {
    dictionaries: Vec<Vec<String>>,
    arrays: Vec<Array<S>>,
}

impl Zarr<FilesystemStore> {
    pub fn open_local(dir: &Path) -> Self {
        Self::open(Arc::new(FilesystemStore::new(dir).expect("a Zarr store")))
    }
}

impl<S: ReadableStorageTraits + ?Sized + 'static> Zarr<S> {
    /// The group, with its dictionaries, and the three arrays' metadata.
    pub fn open(store: Arc<S>) -> Self {
        let group = Group::open(store.clone(), "/").expect("the group");
        Self {
            dictionaries: dictionaries(group.attributes()),
            arrays: ARRAYS
                .iter()
                .map(|a| Array::open(store.clone(), &format!("/{a}")).expect("an array"))
                .collect(),
        }
    }

    /// The number of triples that match.
    pub fn count(&self, pattern: &Pattern<'_>) -> u64 {
        let k = plan(pattern);
        let Ok(bound) = bind(&self.dictionaries, pattern) else {
            return 0;
        };
        let id = bound[k].expect("bound");
        let pairs = self.arrays[k]
            .retrieve_chunk_elements_if_exists::<u64>(&[id, 0])
            .expect("a chunk")
            .unwrap_or_default();
        count(k, id, &pairs, bound, &self.dictionaries[2], pattern)
    }
}
