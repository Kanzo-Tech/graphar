"""Apache DataSketches reads what graphar-stats writes in estimation mode.

    cargo run -q -p graphar-stats --example theta | python3 -I crates/graphar-stats/tests/read_sketch.py

Needs `pip install datasketches==5.2.0`. The exact forms are held byte for byte by tests/sketch.rs.
"""

import sys

from datasketches import compact_theta_sketch, update_theta_sketch

hex_bytes, estimate = sys.stdin.read().split()
ours = compact_theta_sketch.deserialize(bytes.fromhex(hex_bytes))
assert abs(ours.get_estimate() - float(estimate)) < 1e-6, (ours.get_estimate(), estimate)
assert ours.is_estimation_mode()

# The same values through the library: every hash we kept below the library's theta is one the
# library kept, so the two estimate the same set.
theirs = update_theta_sketch()
for i in range(100_000):
    theirs.update(i)
theirs = theirs.compact()
theta = min(ours.theta64, theirs.theta64)
ours_kept = {h for h in ours if h < theta}
theirs_kept = {h for h in theirs if h < theta}
assert ours_kept == theirs_kept, len(ours_kept ^ theirs_kept)
print(f"datasketches reads it: estimate {ours.get_estimate():.0f}, {len(ours_kept)} hashes agree")
