//! A Theta sketch of the distinct values of a column, in the serialized form of Apache
//! DataSketches' compact Theta sketch, which is what Puffin's `apache-datasketches-theta-v1` blob
//! holds.
//!
//! The reference is `datasketches-cpp` (`theta_sketch_impl.hpp`,
//! `compact_theta_sketch_alloc::serialize`, serial version 3) with its defaults: seed 9001,
//! `lg_k` 12 (4096 nominal entries) and sampling probability 1. A value's hash is the first half
//! of `MurmurHash3_x64_128` of its bytes under the seed, shifted right by one; the sketch keeps
//! the `k` smallest, and `theta` is the next one. That is the KMV construction every Theta sketch
//! reduces to on compaction, so any DataSketches reader estimates from it; up to `k` distinct
//! values the sketch is exact and byte for byte what the library writes (the tests check it
//! against the Python bindings).

use std::collections::BTreeSet;

use crate::murmur3;

/// DataSketches' default seed.
pub const DEFAULT_SEED: u64 = 9001;
/// DataSketches' default nominal entries, `2^12`.
pub const NOMINAL_ENTRIES: usize = 4096;
/// `theta` of an exact sketch: every hash is below it.
const MAX_THETA: u64 = i64::MAX as u64;

/// The flags of a compact sketch: read-only (2), compact (8), ordered (16); empty is 4.
const FLAGS: u8 = 2 | 8 | 16;
const EMPTY: u8 = 4;
const SERIAL_VERSION: u8 = 3;
/// `theta_constants::COMPACT`, the family of a compact Theta sketch.
const SKETCH_TYPE: u8 = 3;

/// The 16 bits of a seed's own hash that a serialized sketch carries, so a reader refuses to merge
/// sketches of different seeds: `compute_seed_hash`.
fn seed_hash(seed: u64) -> u16 {
    (murmur3::hash(&seed.to_le_bytes(), 0).0 & 0xffff) as u16
}

/// A KMV Theta sketch: the `k` smallest hashes seen, and `theta`.
#[derive(Clone, Debug)]
pub struct Sketch {
    k: usize,
    seed: u64,
    theta: u64,
    hashes: BTreeSet<u64>,
}

impl Default for Sketch {
    fn default() -> Self {
        Self::new(NOMINAL_ENTRIES, DEFAULT_SEED)
    }
}

impl Sketch {
    /// An empty sketch keeping `k` hashes under `seed`.
    #[must_use]
    pub fn new(k: usize, seed: u64) -> Self {
        Self {
            k: k.max(1),
            seed,
            theta: MAX_THETA,
            hashes: BTreeSet::new(),
        }
    }

    /// Counts a value, given as the bytes Iceberg's single-value serialization makes of it.
    pub fn update(&mut self, bytes: &[u8]) {
        let hash = murmur3::hash(bytes, self.seed).0 >> 1;
        // The library drops a zero hash, and every hash at or above theta.
        if hash == 0 || hash >= self.theta || !self.hashes.insert(hash) {
            return;
        }
        if self.hashes.len() > self.k {
            let largest = self.hashes.pop_last().expect("more than k");
            self.theta = largest;
        }
    }

    /// The estimated number of distinct values: exact while no more than `k` were seen.
    #[must_use]
    pub fn estimate(&self) -> f64 {
        estimate(self.hashes.len(), self.theta)
    }

    /// The sketch as DataSketches serializes a compact, ordered Theta sketch.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let estimating = self.theta < MAX_THETA;
        let empty = self.hashes.is_empty() && !estimating;
        let preamble_longs: u8 = if estimating {
            3
        } else if empty || self.hashes.len() == 1 {
            1
        } else {
            2
        };
        let mut out = Vec::with_capacity(8 * (usize::from(preamble_longs) + self.hashes.len()));
        out.extend([preamble_longs, SERIAL_VERSION, SKETCH_TYPE, 0, 0]);
        out.push(if empty { FLAGS | EMPTY } else { FLAGS });
        out.extend(seed_hash(self.seed).to_le_bytes());
        if preamble_longs > 1 {
            out.extend((self.hashes.len() as u32).to_le_bytes());
            out.extend(0u32.to_le_bytes());
        }
        if estimating {
            out.extend(self.theta.to_le_bytes());
        }
        for hash in &self.hashes {
            out.extend(hash.to_le_bytes());
        }
        out
    }
}

/// The estimate a reader makes of a serialized compact sketch: its entries over theta's share of
/// the hash space. `None` when the bytes are not one.
#[must_use]
pub fn estimate_serialized(bytes: &[u8]) -> Option<f64> {
    let preamble_longs = usize::from(*bytes.first()? & 0x3f);
    if bytes.get(1) != Some(&SERIAL_VERSION) || bytes.get(2) != Some(&SKETCH_TYPE) {
        return None;
    }
    if bytes.get(5)? & EMPTY != 0 {
        return Some(0.0);
    }
    let long = |i: usize| -> Option<u64> {
        Some(u64::from_le_bytes(
            bytes.get(8 * i..8 * i + 8)?.try_into().ok()?,
        ))
    };
    let (entries, theta) = match preamble_longs {
        1 => (1, MAX_THETA),
        2 => (usize::try_from(long(1)? & 0xffff_ffff).ok()?, MAX_THETA),
        3 => (usize::try_from(long(1)? & 0xffff_ffff).ok()?, long(2)?),
        _ => return None,
    };
    (bytes.len() == 8 * (preamble_longs + entries)).then(|| estimate(entries, theta))
}

#[allow(clippy::cast_precision_loss)]
fn estimate(entries: usize, theta: u64) -> f64 {
    entries as f64 / (theta as f64 / MAX_THETA as f64)
}
