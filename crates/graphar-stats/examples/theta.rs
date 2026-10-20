//! The sketch of the integers 0..100000, as hex, then its estimate: what `tests/read_sketch.py`
//! hands Apache DataSketches' Python bindings to read back.

fn main() {
    let sketch = graphar_stats::theta((0..100_000i64).map(i64::to_le_bytes));
    let hex: String = sketch
        .serialize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    println!("{hex}");
    println!("{}", sketch.estimate());
}
