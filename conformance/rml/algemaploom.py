"""The plan of a graph's mapping.ttl as AlgeMapLoom translates it, against the plan graphar-rdf wrote.

    python3 -I conformance/rml/algemaploom.py <graph dir> <translator-bin>

AlgeMapLoom (RMLio/mappingloom-rs) translates an RML document into a plan in the algebra of Min Oo
and Hartig, the one graphar-rdf's mapping is a serialization of. Each of its pipelines is a source
operator, an extend operator and a serializer: this checks that there is one per triples map, that
its source is the map's iterator, and that the triples its serializer writes, with each variable
replaced by the extension function that binds it, are the map's subject, classes and
predicate-object maps. Prints a JSON report and exits non-zero on any difference.

The document is translated **as written**, but for what every processor needs: a database for the
abstract rml:source (finding F4 of conformance/rml/run.py), which AlgeMapLoom also needs typed (it
unwraps the type of the source), and a base IRI for the triples maps' relative names.

Needs: rdflib.
"""

import argparse
import json
import subprocess
import sys
import tempfile
from pathlib import Path

from rdflib import RDF, BNode, Graph, Literal, URIRef

RML = "http://w3id.org/rml/"
D2RQ = "http://www.wiwiss.fu-berlin.de/suhl/bizer/D2RQ/0.1#"
BASE = "http://graphar-rdf.invalid/mapping.ttl"


def rml(name):
    return URIRef(RML + name)


def term(g, node):
    """A term map as one string: the function and the attributes or constants it reads."""
    ref = g.value(node, rml("reference"))
    kind = g.value(node, rml("termType"))
    if kind == rml("IRI"):
        return f"toIRI({ref})"
    dt = g.value(node, rml("datatype"))
    lang = g.value(node, rml("languageMap"))
    if lang is not None:
        return f"toLangLiteral({ref}, {g.value(lang, rml('reference'))})"
    return f"toLiteral({ref}, {dt})"


def written(g):
    """Per triples map: its iterator and the triples it writes, as strings."""
    out = {}
    for tm in g.subjects(RDF.type, rml("TriplesMap")):
        ls = g.value(tm, rml("logicalSource"))
        sm = g.value(tm, rml("subjectMap"))
        s = term(g, sm)
        triples = {f"{s} <{RDF.type}> <{c}>" for c in g.objects(sm, rml("class"))}
        for pom in g.objects(tm, rml("predicateObjectMap")):
            p = g.value(pom, rml("predicate"))
            om = g.value(pom, rml("objectMap"))
            triples.add(f"{s} <{p}> {term(g, om)}")
        out[str(tm)] = {"iterator": str(g.value(ls, rml("iterator"))), "triples": sorted(triples)}
    return out


def function(f):
    """An AlgeMapLoom extension function as the string `term` gives the same term map."""
    if f is None:
        return None
    kind = f["type"]
    if kind == "Reference":
        return f["value"]
    if kind == "Constant":
        return f["value"]
    if kind == "Iri" and f["inner_function"]["type"] == "Constant":
        return f["inner_function"]["value"]
    if kind == "Iri":
        return f"toIRI({function(f['inner_function'])})"
    if kind == "Literal":
        value = function(f["inner_function"])
        lang = function(f.get("langtype_function"))
        if lang is not None:
            return f"toLangLiteral({value}, {lang})"
        return f"toLiteral({value}, {function(f.get('dtype_function'))})"
    raise ValueError(f"an extension function this check does not read: {kind}")


def translated(plan):
    """Per pipeline: its source's iterator and the triples its serializer writes."""
    nodes = plan["nodes"]
    after = {}
    for a, b, _ in plan["edges"]:
        after.setdefault(a, []).append(b)
    out = []
    for i, n in enumerate(nodes):
        if n["operator"]["type"] != "SourceOp":
            continue
        [e] = after[i]
        [s] = after[e]
        extend, serializer = nodes[e]["operator"], nodes[s]["operator"]
        assert extend["type"] == "ExtendOp" and serializer["type"] == "SerializerOp", (extend, serializer)
        bound = {f"?{k}": function(v) for k, v in extend["config"].items()}
        triples = set()
        for line in serializer["config"]["template"].splitlines():
            s_, p, o, dot = line.split()
            assert dot == ".", line
            triples.add(" ".join(bound.get(t, t) for t in (s_, p, o)))
        iterator = n["operator"]["config"]["root_iterator"]["reference"]
        out.append({"iterator": iterator, "triples": sorted(triples)})
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("graph", type=Path)
    ap.add_argument("translator", type=Path)
    args = ap.parse_args()
    g = Graph()
    g.parse(str(args.graph / "mapping.ttl"), format="turtle", publicID=BASE)
    expected = written(g)
    # F4: a database for the abstract source, typed.
    for ls, source in list(g.subject_objects(rml("source"))):
        db = BNode()
        g.set((ls, rml("source"), db))
        g.add((db, RDF.type, rml("Source")))
        g.add((db, RDF.type, URIRef(D2RQ + "Database")))
        g.add((db, URIRef(D2RQ + "jdbcDSN"), Literal("jdbc:duckdb:")))
    with tempfile.TemporaryDirectory() as tmp:
        doc = Path(tmp) / "mapping.ttl"
        g.serialize(str(doc), format="turtle")
        # It writes mapping.dot and mapping.json into its working directory.
        run = subprocess.run(
            [str(args.translator.resolve()), "file", str(doc)], capture_output=True, text=True, cwd=tmp
        )
        out = Path(tmp) / "mapping.json"
        if not out.exists():
            print(json.dumps({"error": (run.stdout + run.stderr)[-1500:]}, indent=1))
            sys.exit(1)
        plan = json.loads(out.read_text())
    got = translated(plan)
    unmatched = list(got)
    missing = {}
    for tm, want in expected.items():
        if want in unmatched:
            unmatched.remove(want)
        else:
            missing[tm] = want
    report = {"triples maps": len(expected), "pipelines": len(got), "equal": not missing and not unmatched}
    if not report["equal"]:
        report["missing"] = missing
        report["extra"] = unmatched
    print(json.dumps(report, indent=1, ensure_ascii=False))
    sys.exit(0 if report["equal"] else 1)


if __name__ == "__main__":
    main()
