//! What a GraphAr vertex table holds, counted once, by the writer, on the rows it is writing.
//!
//! Two places, each the one a reader of that kind already opens:
//!
//! - **Each vertex chunk's Parquet footer** carries, under [`FOOTER_KEY`] in its
//!   `key_value_metadata`, a versioned JSON document: per property, the exact distinct count of
//!   the chunk and its ten commonest values with their frequencies. Nulls and bounds are
//!   Parquet's own `Statistics`, which the Parquet writer counts. [`footer`] is the
//!   [`graphar_parquet::Footer`] that writes it.
//! - **A Puffin file beside the table** ([`puffin`], at [`puffin_path`]) holds one
//!   `apache-datasketches-theta-v1` blob per property, over every chunk: a distinct count does not
//!   combine across chunks (the count of a union lies between the largest part and the sum), and a
//!   Theta sketch does. Iceberg keeps its sketches there for the same reason.
//!
//! **Neither is GraphAr's.** The specification names no footer key and no file beside a table; a
//! reader that does not know them loses nothing a GraphAr or Parquet reader needs, and one that
//! does checks the document's `version` and refuses another.
//!
//! A property is counted over its values: a set or list over its **elements**, as Parquet counts
//! leaf values, and a value that is a struct (a language-tagged string, `STRUCT<value, lang>`) by
//! its first field, the lexical form. A value is compared and reported as Arrow's text of it,
//! which for every GraphAr type is the lexical form RDF gives it.

mod murmur3;
pub mod puffin;
pub mod theta;

use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::cast::AsArray;
use arrow_array::{Array, ArrayRef, RecordBatch};
use arrow_schema::{DataType, TimeUnit};
use graphar_info::{Cardinality, Property, PropertyGroup, VertexInfo};
use parquet::file::metadata::KeyValue;
use serde::{Deserialize, Serialize, Serializer};

pub use theta::Sketch;

/// The footer key a vertex chunk's counts are under, namespaced by this crate's name the way Arrow
/// namespaces its own `ARROW:schema`.
pub const FOOTER_KEY: &str = "graphar-stats";

/// The version of the document under [`FOOTER_KEY`]. A reader checks it and refuses another.
pub const VERSION: u32 = 1;

/// The most common values a column reports.
pub const TOP_VALUES: usize = 10;

/// The longest value, in bytes, reported among the commonest: a longer one is left out rather
/// than cut, since a cut value is one the column does not hold.
pub const LONGEST_VALUE: usize = 64;

/// What went wrong counting.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A column could not be read as text.
    #[error(transparent)]
    Arrow(#[from] arrow_schema::ArrowError),
    /// A document did not (de)serialize.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// A Puffin file that is not one this crate reads.
    #[error("not a Puffin file: {0}")]
    Puffin(String),
    /// A column the information file names is not in the batch.
    #[error("no column `{0}`")]
    Missing(String),
}

/// The document under [`FOOTER_KEY`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    /// [`VERSION`].
    pub version: u32,
    /// Each property's counts, by its name, in the property group's order.
    #[serde(serialize_with = "in_order", deserialize_with = "from_map")]
    pub columns: Vec<(String, ColumnStats)>,
}

/// One property's counts over one chunk. The commonest values are two arrays of one length, as
/// PostgreSQL's `pg_stats` has them, absent when no value repeats.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColumnStats {
    /// The number of distinct non-null values.
    pub distinct_count: u64,
    /// The commonest non-null values occurring at least twice, most frequent first and ties by
    /// value, at most [`TOP_VALUES`], none longer than [`LONGEST_VALUE`] bytes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub most_common_vals: Vec<String>,
    /// Each one's share of the values counted: the chunk's rows for a single value, nulls among
    /// them, and its elements for a set or list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub most_common_freqs: Vec<f64>,
}

fn in_order<S: Serializer>(columns: &[(String, ColumnStats)], s: S) -> Result<S::Ok, S::Error> {
    s.collect_map(columns.iter().map(|(k, v)| (k, v)))
}

fn from_map<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<(String, ColumnStats)>, D::Error> {
    // serde_json keeps a map's order only with `preserve_order`; the order is the information
    // file's, so a reader that needs it has it there.
    let map: std::collections::BTreeMap<String, ColumnStats> = Deserialize::deserialize(d)?;
    Ok(map.into_iter().collect())
}

/// The values a property is counted over: a list's elements, and a struct's first field.
///
/// A list's elements are the range its offsets span, not its whole child array: a chunk is a
/// slice of the table, and its child array is the table's.
fn elements(array: &ArrayRef) -> ArrayRef {
    fn spanned<O: arrow_array::OffsetSizeTrait>(
        list: &arrow_array::GenericListArray<O>,
    ) -> ArrayRef {
        let offsets = list.value_offsets();
        let (first, last) = (offsets[0].as_usize(), offsets[offsets.len() - 1].as_usize());
        list.values().slice(first, last - first)
    }
    let values = match array.data_type() {
        DataType::List(_) => spanned(array.as_list::<i32>()),
        DataType::LargeList(_) => spanned(array.as_list::<i64>()),
        _ => Arc::clone(array),
    };
    match values.data_type() {
        DataType::Struct(_) => Arc::clone(values.as_struct().column(0)),
        _ => values,
    }
}

/// The values of `array` as text, `None` for a null.
fn texts(array: &ArrayRef) -> Result<Vec<Option<String>>, Error> {
    let text = arrow_cast::cast(array, &DataType::Utf8)?;
    let text = text.as_string::<i32>();
    Ok((0..text.len())
        .map(|i| (!text.is_null(i)).then(|| text.value(i).to_owned()))
        .collect())
}

/// One property's counts over `array`.
fn column_stats(property: &Property, array: &ArrayRef) -> Result<ColumnStats, Error> {
    let plain = property.cardinality == Cardinality::Single
        && !matches!(array.data_type(), DataType::Struct(_));
    let values = if plain {
        Arc::clone(array)
    } else {
        elements(array)
    };
    let of = values.len().max(1);
    let mut counts: HashMap<String, u64> = HashMap::new();
    for value in texts(&values)?.into_iter().flatten() {
        *counts.entry(value).or_default() += 1;
    }
    let mut common: Vec<(String, u64)> = counts
        .iter()
        .filter(|(v, n)| **n >= 2 && v.len() <= LONGEST_VALUE)
        .map(|(v, &n)| (v.clone(), n))
        .collect();
    common.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    common.truncate(TOP_VALUES);
    #[allow(clippy::cast_precision_loss)]
    let (vals, freqs) = common
        .into_iter()
        .map(|(v, n)| (v, n as f64 / of as f64))
        .unzip();
    Ok(ColumnStats {
        distinct_count: counts.len() as u64,
        most_common_vals: vals,
        most_common_freqs: freqs,
    })
}

/// The counts of one chunk of `group`: `batch` holds a column per property, by name.
///
/// # Errors
/// [`Error::Missing`] when a property has no column, and [`Error::Arrow`] when one has no text.
pub fn chunk_stats(group: &PropertyGroup, batch: &RecordBatch) -> Result<Stats, Error> {
    let columns = group
        .properties
        .iter()
        .map(|p| {
            let array = batch
                .column_by_name(&p.name)
                .ok_or_else(|| Error::Missing(p.name.clone()))?;
            Ok((p.name.clone(), column_stats(p, array)?))
        })
        .collect::<Result<_, Error>>()?;
    Ok(Stats {
        version: VERSION,
        columns,
    })
}

/// The [`graphar_parquet::Footer`] that writes each vertex chunk's [`Stats`] under
/// [`FOOTER_KEY`].
#[must_use]
pub fn footer() -> graphar_parquet::Footer {
    graphar_parquet::Footer(Arc::new(|_info, group, batch| {
        let stats = chunk_stats(group, batch).map_err(|e| e.to_string())?;
        let json = serde_json::to_string(&stats).map_err(|e| e.to_string())?;
        Ok(vec![KeyValue::new(FOOTER_KEY.to_owned(), json)])
    }))
}

/// Where a vertex type's Puffin file is: `stats.puffin` under the type's prefix, beside its
/// property groups.
#[must_use]
pub fn puffin_path(info: &VertexInfo) -> String {
    format!("{}stats.puffin", info.prefix)
}

/// The bytes Iceberg's single-value serialization makes of each value of `array` (its
/// "Appendix D"): integers, floats and dates little-endian at their width, a boolean as one byte,
/// a timestamp as microseconds, and anything else as the UTF-8 of its text.
fn value_bytes(array: &ArrayRef) -> Result<Vec<Option<Vec<u8>>>, Error> {
    let each = |f: &dyn Fn(usize) -> Vec<u8>| -> Vec<Option<Vec<u8>>> {
        (0..array.len())
            .map(|i| (!array.is_null(i)).then(|| f(i)))
            .collect()
    };
    use arrow_array::types::{
        Date32Type, Float32Type, Float64Type, Int32Type, Int64Type, TimestampMillisecondType,
    };
    Ok(match array.data_type() {
        DataType::Boolean => each(&|i| vec![u8::from(array.as_boolean().value(i))]),
        DataType::Int32 => each(&|i| {
            array
                .as_primitive::<Int32Type>()
                .value(i)
                .to_le_bytes()
                .to_vec()
        }),
        DataType::Int64 => each(&|i| {
            array
                .as_primitive::<Int64Type>()
                .value(i)
                .to_le_bytes()
                .to_vec()
        }),
        DataType::Float32 => each(&|i| {
            array
                .as_primitive::<Float32Type>()
                .value(i)
                .to_le_bytes()
                .to_vec()
        }),
        DataType::Float64 => each(&|i| {
            array
                .as_primitive::<Float64Type>()
                .value(i)
                .to_le_bytes()
                .to_vec()
        }),
        DataType::Date32 => each(&|i| {
            array
                .as_primitive::<Date32Type>()
                .value(i)
                .to_le_bytes()
                .to_vec()
        }),
        DataType::Timestamp(TimeUnit::Millisecond, _) => each(&|i| {
            let micros = array.as_primitive::<TimestampMillisecondType>().value(i) * 1000;
            micros.to_le_bytes().to_vec()
        }),
        _ => texts(array)?
            .into_iter()
            .map(|t| t.map(String::into_bytes))
            .collect(),
    })
}

/// A vertex type's Puffin file: one Theta sketch per property of every group, over `table`, the
/// type's whole table (a column per property, by name), each with its `ndv`.
///
/// # Errors
/// [`Error::Missing`] when a property has no column.
pub fn puffin(info: &VertexInfo, table: &RecordBatch) -> Result<Vec<u8>, Error> {
    let mut blobs = Vec::new();
    let properties = info.property_groups.iter().flat_map(|g| &g.properties);
    for (field, p) in (1..).zip(properties) {
        let array = table
            .column_by_name(&p.name)
            .ok_or_else(|| Error::Missing(p.name.clone()))?;
        let mut sketch = Sketch::default();
        for bytes in value_bytes(&elements(array))?.into_iter().flatten() {
            sketch.update(&bytes);
        }
        blobs.push(puffin::Blob {
            r#type: puffin::THETA.to_owned(),
            fields: vec![field],
            properties: [("ndv".to_owned(), format!("{:.0}", sketch.estimate()))].into(),
            data: sketch.serialize(),
        });
    }
    puffin::write(&blobs, concat!("graphar-stats ", env!("CARGO_PKG_VERSION")))
}
