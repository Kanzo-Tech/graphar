Serialized compact Theta sketches written by Apache DataSketches' Python bindings
(`datasketches` 5.2.0 on PyPI, default `lg_k` 12, seed 9001), as hex:

```python
from datasketches import update_theta_sketch
s = update_theta_sketch()
for i in range(100): s.update(f"v{i}")
open("theta-exact-100.hex", "w").write(s.compact().serialize().hex() + "\n")
s = update_theta_sketch(); s.update("only")
open("theta-one.hex", "w").write(s.compact().serialize().hex() + "\n")
open("theta-empty.hex", "w").write(update_theta_sketch().compact().serialize().hex() + "\n")
```

`tests/sketch.rs` holds `graphar_stats::Sketch` to them byte for byte. `tests/read_sketch.py` (run
by CI) checks the other direction: the library reads what the crate writes in estimation mode.
