//! The synthetic dataset: universities in the shape of LUBM's (Guo, Pan and Heflin, 2005), with
//! the typed literals LUBM lacks (integers, dates, language-tagged labels), generated
//! deterministically from a scale.
//!
//! A scale is a number of universities; each has 15 departments, and each department 10
//! professors, 80 students, 20 courses and 30 publications. The graph conforms to [`SHAPES`] read
//! closed, which is the only graph graphar-rdf writes.

use oxrdf::{Literal, NamedNode, Triple};

const EX: &str = "http://lubm.example.org/";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";

/// The SHACL shapes of the dataset.
pub const SHAPES: &str = r#"@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ub: <http://lubm.example.org/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

ub:UniversityShape a sh:NodeShape ; sh:targetClass ub:University ;
  sh:property [ sh:path ub:name ; sh:datatype xsd:string ; sh:maxCount 1 ] ;
  sh:property [ sh:path ub:founded ; sh:datatype xsd:integer ; sh:maxCount 1 ] .

ub:DepartmentShape a sh:NodeShape ; sh:targetClass ub:Department ;
  sh:property [ sh:path ub:name ; sh:datatype xsd:string ; sh:maxCount 1 ] ;
  sh:property [ sh:path ub:subOrganizationOf ; sh:class ub:University ; sh:maxCount 1 ] .

ub:ProfessorShape a sh:NodeShape ; sh:targetClass ub:Professor ;
  sh:property [ sh:path ub:name ; sh:datatype xsd:string ; sh:maxCount 1 ] ;
  sh:property [ sh:path ub:emailAddress ; sh:datatype xsd:string ; sh:maxCount 1 ] ;
  sh:property [ sh:path ub:age ; sh:datatype xsd:integer ; sh:maxCount 1 ] ;
  sh:property [ sh:path ub:birthDate ; sh:datatype xsd:date ; sh:maxCount 1 ] ;
  sh:property [ sh:path ub:homepage ; sh:nodeKind sh:IRI ; sh:maxCount 1 ] ;
  sh:property [ sh:path rdfs:label ; sh:datatype rdf:langString ] ;
  sh:property [ sh:path ub:worksFor ; sh:class ub:Department ; sh:maxCount 1 ] ;
  sh:property [ sh:path ub:teacherOf ; sh:class ub:Course ] .

ub:StudentShape a sh:NodeShape ; sh:targetClass ub:Student ;
  sh:property [ sh:path ub:name ; sh:datatype xsd:string ; sh:maxCount 1 ] ;
  sh:property [ sh:path ub:emailAddress ; sh:datatype xsd:string ; sh:maxCount 1 ] ;
  sh:property [ sh:path ub:age ; sh:datatype xsd:integer ; sh:maxCount 1 ] ;
  sh:property [ sh:path ub:enrolled ; sh:datatype xsd:date ; sh:maxCount 1 ] ;
  sh:property [ sh:path ub:memberOf ; sh:class ub:Department ; sh:maxCount 1 ] ;
  sh:property [ sh:path ub:advisor ; sh:class ub:Professor ; sh:maxCount 1 ] ;
  sh:property [ sh:path ub:takesCourse ; sh:class ub:Course ] .

ub:CourseShape a sh:NodeShape ; sh:targetClass ub:Course ;
  sh:property [ sh:path ub:name ; sh:datatype xsd:string ; sh:maxCount 1 ] ;
  sh:property [ sh:path ub:credits ; sh:datatype xsd:integer ; sh:maxCount 1 ] ;
  sh:property [ sh:path rdfs:label ; sh:datatype rdf:langString ] .

ub:PublicationShape a sh:NodeShape ; sh:targetClass ub:Publication ;
  sh:property [ sh:path ub:title ; sh:datatype xsd:string ; sh:maxCount 1 ] ;
  sh:property [ sh:path ub:publicationDate ; sh:datatype xsd:date ; sh:maxCount 1 ] ;
  sh:property [ sh:path ub:publicationAuthor ; sh:class ub:Professor ] .
"#;

/// SplitMix64: a small, fixed, portable generator, so a scale is the same graph everywhere.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

struct Out(Vec<Triple>);

impl Out {
    fn iri(&mut self, s: &str, p: &str, o: &str) {
        self.0.push(Triple::new(n(s), n(p), n(o)));
    }
    fn lit(&mut self, s: &str, p: &str, o: Literal) {
        self.0.push(Triple::new(n(s), n(p), o));
    }
}

fn n(iri: &str) -> NamedNode {
    NamedNode::new_unchecked(iri)
}

fn typed(value: impl ToString, local: &str) -> Literal {
    Literal::new_typed_literal(value.to_string(), n(&format!("{XSD}{local}")))
}

fn date(rng: &mut Rng, from: i64, span: u64) -> Literal {
    let y = from + rng.below(span) as i64;
    let m = 1 + rng.below(12);
    let d = 1 + rng.below(28);
    typed(format!("{y:04}-{m:02}-{d:02}"), "date")
}

/// The triples of `universities` universities.
pub fn generate(universities: u32) -> Vec<Triple> {
    let mut rng = Rng(0x5eed_0f9a_aba5);
    let mut out = Out(Vec::new());
    let ex = |local: &str| format!("{EX}{local}");
    let (ty, label) = (RDF_TYPE, RDFS_LABEL);
    for u in 0..universities {
        let uni = format!("{EX}University{u}");
        out.iri(&uni, ty, &ex("University"));
        out.lit(
            &uni,
            &ex("name"),
            Literal::new_simple_literal(format!("University {u}")),
        );
        out.lit(
            &uni,
            &ex("founded"),
            typed(1500 + rng.below(500), "integer"),
        );
        for d in 0..15 {
            let dept = format!("{uni}/Department{d}");
            out.iri(&dept, ty, &ex("Department"));
            out.lit(
                &dept,
                &ex("name"),
                Literal::new_simple_literal(format!("Department {d} of University {u}")),
            );
            out.iri(&dept, &ex("subOrganizationOf"), &uni);
            let courses: Vec<String> = (0..20).map(|c| format!("{dept}/Course{c}")).collect();
            for (c, course) in courses.iter().enumerate() {
                out.iri(course, ty, &ex("Course"));
                out.lit(
                    course,
                    &ex("name"),
                    Literal::new_simple_literal(format!("Course {c}")),
                );
                out.lit(course, &ex("credits"), typed(1 + rng.below(10), "integer"));
                out.lit(
                    course,
                    label,
                    Literal::new_language_tagged_literal_unchecked(format!("Course {c}"), "en"),
                );
                out.lit(
                    course,
                    label,
                    Literal::new_language_tagged_literal_unchecked(format!("Asignatura {c}"), "es"),
                );
            }
            let profs: Vec<String> = (0..10).map(|p| format!("{dept}/Professor{p}")).collect();
            for (p, prof) in profs.iter().enumerate() {
                out.iri(prof, ty, &ex("Professor"));
                out.lit(
                    prof,
                    &ex("name"),
                    Literal::new_simple_literal(format!("Professor {p} of {d}.{u}")),
                );
                out.lit(
                    prof,
                    &ex("emailAddress"),
                    Literal::new_simple_literal(format!("p{p}.d{d}@u{u}.example.org")),
                );
                out.lit(prof, &ex("age"), typed(30 + rng.below(40), "integer"));
                out.lit(prof, &ex("birthDate"), date(&mut rng, 1950, 40));
                out.iri(
                    prof,
                    &ex("homepage"),
                    &format!("http://www.u{u}.example.org/d{d}/p{p}"),
                );
                out.lit(
                    prof,
                    label,
                    Literal::new_language_tagged_literal_unchecked(format!("Prof. {p}"), "en"),
                );
                out.iri(prof, &ex("worksFor"), &dept);
                for _ in 0..2 {
                    out.iri(prof, &ex("teacherOf"), &courses[rng.below(20) as usize]);
                }
            }
            for s in 0..80 {
                let student = format!("{dept}/Student{s}");
                out.iri(&student, ty, &ex("Student"));
                out.lit(
                    &student,
                    &ex("name"),
                    Literal::new_simple_literal(format!("Student {s} of {d}.{u}")),
                );
                out.lit(
                    &student,
                    &ex("emailAddress"),
                    Literal::new_simple_literal(format!("s{s}.d{d}@u{u}.example.org")),
                );
                out.lit(&student, &ex("age"), typed(18 + rng.below(12), "integer"));
                out.lit(&student, &ex("enrolled"), date(&mut rng, 2010, 15));
                out.iri(&student, &ex("memberOf"), &dept);
                out.iri(&student, &ex("advisor"), &profs[rng.below(10) as usize]);
                for _ in 0..(2 + rng.below(3)) {
                    out.iri(
                        &student,
                        &ex("takesCourse"),
                        &courses[rng.below(20) as usize],
                    );
                }
            }
            for p in 0..30 {
                let publication = format!("{dept}/Publication{p}");
                out.iri(&publication, ty, &ex("Publication"));
                out.lit(
                    &publication,
                    &ex("title"),
                    Literal::new_simple_literal(format!("On topic {} ({p})", rng.below(1000))),
                );
                out.lit(
                    &publication,
                    &ex("publicationDate"),
                    date(&mut rng, 1990, 35),
                );
                for _ in 0..(1 + rng.below(3)) {
                    out.iri(
                        &publication,
                        &ex("publicationAuthor"),
                        &profs[rng.below(10) as usize],
                    );
                }
            }
        }
    }
    // A graph is a set: a course drawn twice for one student is one triple.
    let mut seen = std::collections::HashSet::new();
    out.0.retain(|t| seen.insert(t.clone()));
    out.0
}
