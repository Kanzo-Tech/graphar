//! The sketch of the integers 0..100000, as hex, then its estimate: what `tests/read_sketch.py`
//! hands Apache DataSketches to read back.

fn main() {
    let mut sketch = graphar_stats::Sketch::default();
    for i in 0..100_000i64 {
        sketch.update(&i.to_le_bytes());
    }
    let hex: String = sketch
        .serialize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    println!("{hex}");
    println!("{}", sketch.estimate());
}
