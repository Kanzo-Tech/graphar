//! `MurmurHash3_x64_128`, the hash every Apache DataSketches sketch is built on
//! (`MurmurHash3.h` in `datasketches-cpp`, after Austin Appleby's public-domain original).

const C1: u64 = 0x87c3_7b91_1142_53d5;
const C2: u64 = 0x4cf5_ad43_2745_937f;

const fn fmix64(mut k: u64) -> u64 {
    k ^= k >> 33;
    k = k.wrapping_mul(0xff51_afd7_ed55_8ccd);
    k ^= k >> 33;
    k = k.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    k ^ (k >> 33)
}

/// The two 64-bit halves of the hash of `data` under `seed`.
pub(crate) fn hash(data: &[u8], seed: u64) -> (u64, u64) {
    let (mut h1, mut h2) = (seed, seed);
    let blocks = data.chunks_exact(16);
    let tail = blocks.remainder();
    for block in blocks {
        let k1 = u64::from_le_bytes(block[..8].try_into().expect("8 bytes"));
        let k2 = u64::from_le_bytes(block[8..].try_into().expect("8 bytes"));
        h1 ^= k1.wrapping_mul(C1).rotate_left(31).wrapping_mul(C2);
        h1 = h1
            .rotate_left(27)
            .wrapping_add(h2)
            .wrapping_mul(5)
            .wrapping_add(0x52dc_e729);
        h2 ^= k2.wrapping_mul(C2).rotate_left(33).wrapping_mul(C1);
        h2 = h2
            .rotate_left(31)
            .wrapping_add(h1)
            .wrapping_mul(5)
            .wrapping_add(0x3849_5ab5);
    }
    let (mut k1, mut k2) = (0u64, 0u64);
    for (i, &b) in tail.iter().enumerate() {
        if i < 8 {
            k1 ^= u64::from(b) << (i * 8);
        } else {
            k2 ^= u64::from(b) << ((i - 8) * 8);
        }
    }
    if tail.len() > 8 {
        h2 ^= k2.wrapping_mul(C2).rotate_left(33).wrapping_mul(C1);
    }
    if !tail.is_empty() {
        h1 ^= k1.wrapping_mul(C1).rotate_left(31).wrapping_mul(C2);
    }
    let len = data.len() as u64;
    h1 ^= len;
    h2 ^= len;
    h1 = h1.wrapping_add(h2);
    h2 = h2.wrapping_add(h1);
    h1 = fmix64(h1);
    h2 = fmix64(h2);
    h1 = h1.wrapping_add(h2);
    h2 = h2.wrapping_add(h1);
    (h1, h2)
}
