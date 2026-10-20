"""The RDF reading of a graph graphar-rdf wrote, through independent RML processors.

    python3 -I conformance/rml/run.py <graph dir> <expected .ttl|.nt> [--pg DSN] [--rmlmapper JAR]

Loads every vertex and edge table of the graph into DuckDB (and, with --pg, PostgreSQL), runs the
graph's mapping.ttl through Morph-KGC on each and through RMLMapper on PostgreSQL, and compares each
output with the expected triples as RDF graphs (a simple literal as xsd:string, and an integer or
boolean literal in its canonical lexical form, which is how graphar-rdf stores and reads it back). Prints one JSON object per processor and exits non-zero if any differs or fails.

The mapping is run **as written**, but for three defects of Morph-KGC 2.10 that no mapping can
avoid, each a rewrite here (findings F6 to F8 of the run over fossil's programs, 2026-10-19):
F6, it reads only its own rml:tableName/rml:query and ignores RML-IO's rml:iterator with a SQL2008
formulation, giving zero triples and no error; F7, a source column named `subject` is overwritten
(or `predicate`, `object`, `graph`) is overwritten by its own working column of that name; F8, an SQL NULL becomes the literal "None" unless na_values says so.
RMLMapper needs only what every processor needs, a database for the abstract rml:source (F4).

One defect of RMLMapper 8.1.0 is not rewritten but **explained**: a difference counts as one only
if every missing triple has its extra twin and the pair is that defect. F5: a boolean read from
PostgreSQL is written "t"/"f" where R2RML's natural mapping says "true"/"false". A processor whose
differences are all explained is reported `equal: false, explained: true`, and does not fail the
run.

Needs: duckdb, rdflib, morph-kgc==2.10.0, duckdb-engine, psycopg2-binary; Java and the RMLMapper
jar for RMLMapper.
"""

import argparse
import json
import logging
import subprocess
import sys
import tempfile
from pathlib import Path

import duckdb
import rdflib
from rdflib import RDF, BNode, Graph, Literal, URIRef
from rdflib.compare import graph_diff, to_isomorphic

RML = "http://w3id.org/rml/"
D2RQ = "http://www.wiwiss.fu-berlin.de/suhl/bizer/D2RQ/0.1#"
XS = URIRef("http://www.w3.org/2001/XMLSchema#string")
BASE = "http://graphar-rdf.invalid/mapping.ttl"
# Morph-KGC's working columns (F7).
CLASH = ("subject", "predicate", "object", "graph")

# A literal is compared as written, but for the canonical forms `canon` gives the typed ones.
rdflib.NORMALIZE_LITERALS = False
XSD = "http://www.w3.org/2001/XMLSchema#"
INTEGERS = {URIRef(XSD + t) for t in ("integer", "long", "int")}
XSD_BOOLEAN = URIRef(XSD + "boolean")
# rdflib logs every literal it cannot map to a Python value: those are data, kept as written.
logging.getLogger("rdflib").setLevel(logging.CRITICAL)


def canon(g):
    out = Graph()
    for s, p, o in g:
        if isinstance(o, Literal) and o.datatype is None and o.language is None:
            o = Literal(str(o), datatype=XS)
        elif isinstance(o, Literal) and o.datatype in INTEGERS:
            o = Literal(str(int(str(o))), datatype=o.datatype)
        elif isinstance(o, Literal) and o.datatype == XSD_BOOLEAN:
            o = Literal({"1": "true", "0": "false"}.get(str(o), str(o)), datatype=o.datatype)
        out.add((s, p, o))
    return out


def parse(path, fmt=None):
    g = Graph()
    g.parse(str(path), format=fmt or ("nt" if str(path).endswith(".nt") else "turtle"))
    return canon(g)


def tables(graph):
    """Each table of the graph as the Parquet files that hold it."""
    out = {}
    for d in sorted((graph / "vertex").iterdir()):
        out[d.name] = sorted(str(p) for p in (d / "properties").glob("chunk*"))
    if (graph / "edge").exists():
        for d in sorted((graph / "edge").iterdir()):
            out[d.name] = sorted(str(p) for p in (d / "ordered_by_source" / "adj_list").glob("part*/chunk*"))
    return out


def load_duckdb(graph, path):
    con = duckdb.connect(str(path))
    for name, files in tables(graph).items():
        if files:
            con.execute(f'CREATE TABLE "{name}" AS SELECT * FROM read_parquet({files!r})')
    return con


PGTYPE = {"VARCHAR": "text", "BIGINT": "bigint", "INTEGER": "integer", "DATE": "date", "BOOLEAN": "boolean"}


def pgtype(t):
    if t.endswith("[]"):
        return pgtype(t[:-2]) + "[]"
    if t.replace('"', "").startswith("STRUCT(value VARCHAR, lang VARCHAR)"):
        return "langstr"
    return PGTYPE[t]


def load_pg(con, dsn):
    import psycopg2

    db = "graphar_rdf_conformance"
    admin = psycopg2.connect(dsn + " dbname=postgres")
    admin.autocommit = True
    admin.cursor().execute(f'DROP DATABASE IF EXISTS "{db}"')
    admin.cursor().execute(f'CREATE DATABASE "{db}"')
    admin.close()
    pg = psycopg2.connect(dsn + f" dbname={db}")
    cur = pg.cursor()
    cur.execute("CREATE TYPE langstr AS (value text, lang text)")
    for (table,) in con.execute("SELECT table_name FROM information_schema.tables").fetchall():
        cols = con.execute(f'DESCRIBE "{table}"').fetchall()
        types = [pgtype(c[1]) for c in cols]
        cur.execute(f'CREATE TABLE "{table}" (' + ", ".join(f'"{c[0]}" {t}' for c, t in zip(cols, types)) + ")")
        for row in con.execute(f'SELECT * FROM "{table}"').fetchall():
            values = []
            for v, t in zip(row, types):
                if t == "langstr" and v is not None:
                    v = (v["value"], v["lang"])
                elif t == "langstr[]" and v is not None:
                    v = [(x["value"], x["lang"]) for x in v]
                values.append(v)
            # A composite is written with ROW(...)::langstr; psycopg2 adapts a tuple to ROW.
            marks = []
            for v, t in zip(values, types):
                marks.append("%s::langstr" if t == "langstr" else ("%s::langstr[]" if t == "langstr[]" else "%s"))
            cur.execute(f'INSERT INTO "{table}" VALUES (' + ",".join(marks) + ")", values)
    pg.commit()
    pg.close()
    return db


def mapping(graph):
    g = Graph()
    g.parse(str(graph / "mapping.ttl"), format="turtle", publicID=BASE)
    return g


def refs_under(g, node, seen=None):
    seen = seen or set()
    out = set()
    for p, o in g.predicate_objects(node):
        if str(p) == RML + "reference":
            out.add(str(o))
        elif str(p) != RML + "logicalSource" and isinstance(o, BNode) and o not in seen:
            seen.add(o)
            out |= refs_under(g, o, seen)
    return out


def morph_mapping(graph, work):
    """F6 and F7: rml:iterator to rml:query, and the columns Morph-KGC clobbers read under other
    names."""
    g = mapping(graph)
    for tm, ls in list(g.subject_objects(URIRef(RML + "logicalSource"))):
        kind = str(g.value(ls, URIRef(RML + "referenceFormulation"))).rsplit("/", 1)[1]
        it = str(g.value(ls, URIRef(RML + "iterator")))
        inner = f"SELECT * FROM {it}" if kind == "SQL2008Table" else it
        clash = [c for c in CLASH if c in refs_under(g, tm)]
        g.remove((ls, URIRef(RML + "iterator"), None))
        g.remove((ls, URIRef(RML + "source"), None))
        extra = "".join(f', q."{c}" AS "{c}__"' for c in clash)
        g.add((ls, URIRef(RML + "query"), Literal(f"SELECT q.*{extra} FROM ({inner}) AS q")))
        g.set((ls, URIRef(RML + "referenceFormulation"), URIRef(RML + "SQL2008")))
    for node, p, o in list(g.triples((None, URIRef(RML + "reference"), None))):
        if str(o) in CLASH:
            g.set((node, p, Literal(f"{o}__")))
    out = work / "morph.ttl"
    g.serialize(str(out), format="turtle")
    return out


def morph(graph, work, db_url, tag):
    out = work / f"{tag}.nt"
    cfg = work / f"{tag}.ini"
    cfg.write_text(
        "[CONFIGURATION]\n"
        f"output_file={out}\nnumber_of_processes=1\n"
        # F8.
        "na_values=,nan,None\n"
        f"[DataSource1]\nmappings={morph_mapping(graph, work)}\ndb_url={db_url}\n"
    )
    r = subprocess.run([sys.executable, "-m", "morph_kgc", str(cfg)], capture_output=True, text=True, timeout=1800)
    if r.returncode != 0 or not out.exists():
        raise RuntimeError(tail(r.stdout + r.stderr))
    return parse(out, "nt")


def rmlmapper(graph, work, jar, dsn, db):
    g = mapping(graph)
    params = dict(kv.split("=", 1) for kv in dsn.split())
    url = f"jdbc:postgresql://{params.get('host', '127.0.0.1')}:{params.get('port', '5432')}/{db}"
    for src in set(g.objects(None, URIRef(RML + "source"))):
        g.add((src, RDF.type, URIRef(D2RQ + "Database")))
        g.add((src, URIRef(D2RQ + "jdbcDSN"), Literal(url)))
        g.add((src, URIRef(D2RQ + "jdbcDriver"), Literal("org.postgresql.Driver")))
        g.add((src, URIRef(D2RQ + "username"), Literal(params.get("user", "postgres"))))
        g.add((src, URIRef(D2RQ + "password"), Literal(params.get("password", ""))))
    m = work / "rmlmapper.ttl"
    g.serialize(str(m), format="turtle")
    out = work / "rmlmapper.nq"
    r = subprocess.run(["java", "-jar", jar, "-m", str(m), "-o", str(out), "-s", "nquads"],
                       capture_output=True, text=True, timeout=1800)
    if r.returncode != 0 or not out.exists():
        raise RuntimeError(tail(r.stdout + r.stderr))
    return parse(out, "nt")


def tail(s, n=8):
    return "\n".join([l for l in s.strip().splitlines() if l.strip()][-n:])


def defect(want, got):
    """The processor defect that turns the triple `want` into `got`, if one does."""
    if want[:2] != got[:2] or not isinstance(want[2], Literal) or not isinstance(got[2], Literal):
        return None
    w, g = want[2], got[2]
    if w.datatype == g.datatype == XSD_BOOLEAN and (str(w), str(g)) in {("true", "t"), ("false", "f")}:
        return "F5"
    return None


def compare(expected, got):
    _, missing, extra = graph_diff(to_isomorphic(expected), to_isomorphic(got))
    show = lambda g: sorted(" ".join(x.n3() for x in t) for t in g)[:8]
    explained = {}
    unmatched = list(extra)
    for want in missing:
        twin = next(((g, d) for g in unmatched if (d := defect(want, g))), None)
        if twin is None:
            explained = None
            break
        unmatched.remove(twin[0])
        explained[twin[1]] = explained.get(twin[1], 0) + 1
    equal = len(missing) == 0 and len(extra) == 0
    out = {"triples": len(got), "equal": equal}
    if not equal:
        out["explained"] = explained is not None and not unmatched
        out["defects"] = explained or {}
        out["missing"] = show(missing)
        out["extra"] = show(extra)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("graph", type=Path)
    ap.add_argument("expected", type=Path)
    ap.add_argument("--pg", help="libpq DSN without dbname, e.g. 'host=127.0.0.1 port=5432 user=postgres'")
    ap.add_argument("--rmlmapper", help="the RMLMapper jar")
    args = ap.parse_args()
    expected = parse(args.expected)
    report = {"expected": len(expected)}
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        db = work / "graph.duckdb"
        con = load_duckdb(args.graph, db)
        runs = {}
        pgdb = None
        if args.pg:
            pgdb = load_pg(con, args.pg)
        con.close()
        runs["morph-kgc/duckdb"] = lambda: morph(args.graph, work, f"duckdb:///{db}", "morph-duckdb")
        if pgdb:
            params = dict(kv.split("=", 1) for kv in args.pg.split())
            url = (f"postgresql+psycopg2://{params.get('user', 'postgres')}:{params.get('password', '')}"
                   f"@{params.get('host', '127.0.0.1')}:{params.get('port', '5432')}/{pgdb}")
            runs["morph-kgc/postgresql"] = lambda: morph(args.graph, work, url, "morph-pg")
            if args.rmlmapper:
                runs["rmlmapper/postgresql"] = lambda: rmlmapper(args.graph, work, args.rmlmapper, args.pg, pgdb)
        ok = True
        for name, run in runs.items():
            try:
                report[name] = compare(expected, run())
            except Exception as e:  # noqa: BLE001 - every failure is a result to report
                report[name] = {"error": str(e)[-800:]}
            ok = ok and (report[name].get("equal") or report[name].get("explained", False))
    print(json.dumps(report, indent=1, ensure_ascii=False))
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
