"""GraphAr's own C++ library opens what graphar-parquet wrote.

The reader is `graphar` on PyPI (0.13.0.dev1): pybind11 bindings over GraphAr's C++ library
(`libgraphar.so`, Arrow C++ 22), so the YAML goes through mini-yaml and every chunk, count and
offset file through C++'s readers. Its `property()` returns `int64` and `string` values only, so
those are compared; for the other types `IsValid` is, which makes C++ decode the chunk with its
own type dispatch (`TryToCastToAny`, `graph_reader.cc`) — a type it does not hold throws.

The expected values are `crates/graphar-parquet/tests/sample/mod.rs`, restated: this file is the
oracle, so it does not read them from the writer it checks.

    python conformance/read_cpp.py conformance/sample/sample.graph.yml
"""

import os
import sys

import graphar
from graphar._core import AdjListType

PERSONS = ["ana", "bo", "cy", "di", "ed", "flo", "gus"]
KNOWS = [(0, 1), (2, 6), (0, 2), (6, 5), (1, 2), (0, 6), (6, 0), (1, 0)]
CREATED = [(6, 2), (0, 0), (2, 1), (6, 0)]


def check(what, got, expected):
    if got != expected:
        sys.exit(f"{what}: C++ read {got!r}, expected {expected!r}")
    print(f"ok  {what}")


def valid(vertex, names):
    return [vertex.IsValid(n) for n in names]


def main(path):
    g = graphar.GraphInfo.load(os.path.abspath(path))
    check("graph is validated", g.is_validated(), True)
    check("graph name", g.get_name(), "sample")
    # No `prefix` written: C++ defaults it to the graph document's directory.
    check("prefix", g.get_prefix(), os.path.dirname(os.path.abspath(path)) + "/")
    check("vertex types", [v.get_type() for v in g.get_vertex_infos()], ["person", "software", "nickname"])
    check("edge types", [(e.get_src_type(), e.get_edge_type(), e.get_dst_type()) for e in g.get_edge_infos()],
          [("person", "knows", "person"), ("person", "created", "software")])
    check("extra_info", g.get_extra_info(), {"written_by": "graphar-parquet"})
    person = g.get_vertex_info("person")
    check("person primary key", [person.is_primary_key("id"), person.is_primary_key("name")], [True, False])
    check("person nullable", [person.is_nullable_key(n) for n in ["name", "age"]], [False, True])

    persons = graphar.VerticesCollection.Make(g, "person")
    check("person count", persons.size(), 7)
    rows = [(v.id(), v.property("id"), v.property("name"), valid(v, ["age", "born", "active"])) for v in persons]
    nulls = [[True] * 3, [True] * 3, [False, True, True], [True, False, True], [True, True, False],
             [True] * 3, [True] * 3]
    check("person rows", rows, [(i, 100 + i, PERSONS[i], nulls[i]) for i in range(7)])

    software = graphar.VerticesCollection.Make(g, "software")
    check("software rows", [(v.id(), v.property("id"), v.property("name"), valid(v, ["price", "released"]))
                            for v in software],
          [(0, 900, "lop", [True, True]), (1, 901, "ripple", [False, True]), (2, 902, "nebula", [True, False])])
    check("released, a timestamp, read as int64 milliseconds",
          [v.property("released") for v in software if v.IsValid("released")], [1_700_000_000_123, 0])

    nickname = graphar.VerticesCollection.Make(g, "nickname")
    check("nickname rows (no primary key)", [(v.property("src"), v.property("value")) for v in nickname],
          [(0, "annie"), (0, "an"), (3, "dee")])

    knows_by_src = sorted(KNOWS, key=lambda e: e[0])  # stable, as the writer sorts
    knows_by_dst_part = sorted(KNOWS, key=lambda e: e[1] // 3)
    for adj, expected in [(AdjListType.ordered_by_source, knows_by_src),
                          (AdjListType.unordered_by_dest, knows_by_dst_part)]:
        edges = graphar.EdgesCollection.Make(g, "person", "knows", "person", adj)
        check(f"knows {adj.name} count", edges.size(), len(KNOWS))
        check(f"knows {adj.name} edges", [(e.source(), e.destination(), e.IsValid("weight")) for e in edges],
              [(s, d, True) for s, d in expected])

    created = graphar.EdgesCollection.Make(g, "person", "created", "software", AdjListType.ordered_by_source)
    check("created edges", [(e.source(), e.destination()) for e in created], sorted(CREATED, key=lambda e: e[0]))
    # `find_src` reads the offset chunk of the source's part.
    it = created.find_src(6, created.begin())
    check("created find_src(6) through the offsets", (it.source(), it.destination()), (6, 2))


if __name__ == "__main__":
    main(sys.argv[1])
