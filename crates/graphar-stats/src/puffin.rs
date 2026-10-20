//! Puffin, Apache Iceberg's file of blobs about a table (`format/puffin-spec.md`): the magic,
//! the blobs, and a footer that is the magic, a JSON payload, its length, four bytes of flags and
//! the magic again.
//!
//! **Two departures, because the blobs describe a GraphAr table and not an Iceberg snapshot.** The
//! specification makes `snapshot-id` and `sequence-number` required ("ID of the Iceberg table's
//! snapshot the blob was computed from", "Sequence number of the Iceberg table's snapshot"); there
//! is no snapshot, so both are written `-1` and never read. `fields` holds "field IDs the blob was
//! computed for"; a GraphAr property has no field ID, so it is the property's position among its
//! vertex type's properties, the property groups in order, counted from 1 as Iceberg's IDs are.

use serde::{Deserialize, Serialize};

use crate::Error;

const MAGIC: [u8; 4] = [0x50, 0x46, 0x41, 0x31];

/// The blob type of a Theta sketch (`apache-datasketches-theta-v1`): "A serialized form of a
/// 'compact' Theta sketch produced by the Apache DataSketches library", with the `ndv` property.
pub const THETA: &str = "apache-datasketches-theta-v1";

/// A blob's entry in the footer, as the specification names its fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BlobMetadata {
    /// The blob's type, as [`THETA`].
    pub r#type: String,
    /// The columns it was computed for (see the module comment).
    pub fields: Vec<i32>,
    /// Always `-1` here (see the module comment).
    pub snapshot_id: i64,
    /// Always `-1` here (see the module comment).
    pub sequence_number: i64,
    /// Where the blob starts in the file.
    pub offset: i64,
    /// Its length in bytes.
    pub length: i64,
    /// Absent: the blobs are written uncompressed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression_codec: Option<String>,
    /// For [`THETA`], `ndv`: the sketch's estimate, rounded, as text.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub properties: std::collections::BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct FileMetadata {
    blobs: Vec<BlobMetadata>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    properties: std::collections::BTreeMap<String, String>,
}

/// One blob to write: its type, fields, properties and bytes.
#[derive(Clone, Debug)]
pub struct Blob {
    /// The blob's type.
    pub r#type: String,
    /// The columns it was computed for.
    pub fields: Vec<i32>,
    /// Its properties.
    pub properties: std::collections::BTreeMap<String, String>,
    /// Its bytes, uncompressed.
    pub data: Vec<u8>,
}

/// A Puffin file of `blobs`, its footer saying it was `created_by` that writer.
///
/// # Errors
/// [`Error::Json`] if the footer does not serialize, which no blob makes happen.
pub fn write(blobs: &[Blob], created_by: &str) -> Result<Vec<u8>, Error> {
    let mut out = MAGIC.to_vec();
    let mut metadata = Vec::with_capacity(blobs.len());
    for blob in blobs {
        metadata.push(BlobMetadata {
            r#type: blob.r#type.clone(),
            fields: blob.fields.clone(),
            snapshot_id: -1,
            sequence_number: -1,
            offset: out.len() as i64,
            length: blob.data.len() as i64,
            compression_codec: None,
            properties: blob.properties.clone(),
        });
        out.extend(&blob.data);
    }
    let payload = serde_json::to_vec(&FileMetadata {
        blobs: metadata,
        properties: [("created-by".to_owned(), created_by.to_owned())].into(),
    })?;
    out.extend(MAGIC);
    out.extend(&payload);
    out.extend((payload.len() as i32).to_le_bytes());
    out.extend([0u8; 4]);
    out.extend(MAGIC);
    Ok(out)
}

/// The blobs of a Puffin file, each with its bytes.
///
/// # Errors
/// [`Error::Puffin`] when the bytes are not an uncompressed Puffin file.
pub fn read(bytes: &[u8]) -> Result<Vec<(BlobMetadata, &[u8])>, Error> {
    let bad = |why: &str| Error::Puffin(why.to_owned());
    let n = bytes.len();
    if n < 16 || bytes[..4] != MAGIC || bytes[n - 4..] != MAGIC {
        return Err(bad("no Puffin magic at both ends"));
    }
    if bytes[n - 8] & 1 != 0 {
        return Err(bad(
            "a compressed footer payload, which this reader does not decode",
        ));
    }
    let size = i32::from_le_bytes(bytes[n - 12..n - 8].try_into().expect("4 bytes"));
    let size = usize::try_from(size).map_err(|_| bad("a negative footer size"))?;
    let start = n
        .checked_sub(12 + size + 4)
        .filter(|&s| s >= 4 && bytes[s..s + 4] == MAGIC)
        .ok_or_else(|| bad("the footer is not where its size puts it"))?;
    let meta: FileMetadata = serde_json::from_slice(&bytes[start + 4..start + 4 + size])?;
    meta.blobs
        .into_iter()
        .map(|b| {
            if b.compression_codec.is_some() {
                return Err(bad("a compressed blob, which this reader does not decode"));
            }
            let from = usize::try_from(b.offset).map_err(|_| bad("a negative offset"))?;
            let to = from + usize::try_from(b.length).map_err(|_| bad("a negative length"))?;
            let data = bytes
                .get(from..to)
                .ok_or_else(|| bad("a blob past the file"))?;
            Ok((b, data))
        })
        .collect()
}
