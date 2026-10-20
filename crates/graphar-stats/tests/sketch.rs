//! `Sketch` against what Apache DataSketches writes (`tests/fixtures/README.md`).

use graphar_stats::Sketch;
use graphar_stats::theta::estimate_serialized;

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let hex = std::fs::read_to_string(path).expect("the fixture");
    let hex = hex.trim();
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect()
}

#[test]
fn an_exact_sketch_is_byte_for_byte_the_librarys() {
    let mut sketch = Sketch::default();
    for i in 0..100 {
        sketch.update(format!("v{i}").as_bytes());
    }
    // Twice: a value seen again changes nothing.
    for i in 0..100 {
        sketch.update(format!("v{i}").as_bytes());
    }
    assert_eq!(sketch.serialize(), fixture("theta-exact-100.hex"));
    assert_eq!(sketch.estimate(), 100.0);
}

#[test]
fn one_value_and_none_are_the_librarys_short_forms() {
    let mut one = Sketch::default();
    one.update(b"only");
    assert_eq!(one.serialize(), fixture("theta-one.hex"));
    assert_eq!(Sketch::default().serialize(), fixture("theta-empty.hex"));
    assert_eq!(estimate_serialized(&fixture("theta-empty.hex")), Some(0.0));
    assert_eq!(estimate_serialized(&fixture("theta-one.hex")), Some(1.0));
}

#[test]
fn past_k_values_the_sketch_estimates_within_its_error() {
    let mut sketch = Sketch::default();
    for i in 0..100_000i64 {
        sketch.update(&i.to_le_bytes());
    }
    let estimate = sketch.estimate();
    // The relative standard error of a KMV sketch is 1/sqrt(k - 2), 1.6 % at k = 4096; three of
    // them is 4.7 %.
    assert!((estimate / 100_000.0 - 1.0).abs() < 0.047, "{estimate}");
    let bytes = sketch.serialize();
    assert_eq!(bytes[0], 3, "estimation mode has three preamble longs");
    assert_eq!(estimate_serialized(&bytes), Some(estimate));
}
