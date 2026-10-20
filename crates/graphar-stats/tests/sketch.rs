//! `theta` against what Apache DataSketches' Python bindings write (`tests/fixtures/README.md`):
//! the Rust port hashes a value's bytes as the other bindings do.

use datasketches::theta::CompactThetaSketch;
use graphar_stats::theta;

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let hex = std::fs::read_to_string(path).expect("the fixture");
    let hex = hex.trim();
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect()
}

fn estimate(bytes: &[u8]) -> f64 {
    CompactThetaSketch::deserialize(bytes)
        .expect("a compact sketch")
        .estimate()
}

#[test]
fn an_exact_sketch_is_byte_for_byte_the_python_bindings() {
    // Twice: a value seen again changes nothing.
    let values = (0..200).map(|i| format!("v{}", i % 100));
    let sketch = theta(values);
    assert_eq!(sketch.serialize(), fixture("theta-exact-100.hex"));
    assert_eq!(sketch.estimate(), 100.0);
}

#[test]
fn one_value_and_none_are_the_python_bindings_short_forms() {
    assert_eq!(theta(["only"]).serialize(), fixture("theta-one.hex"));
    assert_eq!(theta::<&[u8]>([]).serialize(), fixture("theta-empty.hex"));
    assert_eq!(estimate(&fixture("theta-empty.hex")), 0.0);
    assert_eq!(estimate(&fixture("theta-one.hex")), 1.0);
}

#[test]
fn past_k_values_the_sketch_estimates_within_its_error() {
    let sketch = theta((0..100_000i64).map(i64::to_le_bytes));
    let e = sketch.estimate();
    // The relative standard error at k = 4096 is about 1.6 %; three of them is 4.7 %.
    assert!((e / 100_000.0 - 1.0).abs() < 0.047, "{e}");
    assert!(sketch.is_estimation_mode());
    assert_eq!(estimate(&sketch.serialize()), e);
}
