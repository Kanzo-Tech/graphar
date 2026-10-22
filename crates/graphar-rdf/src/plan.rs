//! The RDF reading of a [`Schema`] as a plan in the mapping algebra of Min Oo and Hartig ("An
//! Algebraic Foundation for Knowledge Graph Construction", ESWC 2025; extended version
//! arXiv:2503.10385). [`read()`](crate::read()) evaluates it, and `mapping.ttl` (RML-Core) and
//! `mapping.r2rml.ttl` (R2RML) are two serializations of it, so the three cannot disagree.
//!
//! **A plan is in the algebra's normal form**: a union of branches, each
//! `π{s,p,o}(ε_o(ε_p(ε_s(Source))))` — a source relation, the three term attributes the output
//! function reads (`a_s`, `a_p`, `a_o`, Definition 3) extended onto it, and the projection to them.
//! A [`Map`] is the branches that share a source and a subject, which is what a triples map is; its
//! `Display` writes every branch out.
//!
//! **One extension function is not the algebra's.** It leaves language tags out (its footnote 3);
//! [`Expr::LangLiteral`] is `toLangLiteral(value, lang)`, a language-tagged literal of two
//! attributes, which RML-Core writes as `rml:languageMap` and R2RML, whose language is a constant,
//! as one triples map per language the column holds.
//!
//! **A source is a relation over the tables, named as SQL:2008 renders it.** Every identifier is
//! delimited and every query standard SQL — `unnest` is the feature S091 — because three
//! processors on two engines disagreed with an earlier generator on exactly that (`rml-motores`,
//! findings F1 to F3). No attribute a term reads is an internal id, a row position or a generated
//! blank node: the ids appear only in the edge sources' joins, so the triples do not depend on the
//! layout.

use std::fmt;

use crate::schema::{Cardinality, Error, Object, Schema, Storage};
use crate::{RDF_LANG_STRING, RDF_TYPE, SUBJECT, XSD_STRING};

/// The plan of a schema: a union of [`Map`]s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The branches, grouped by source and subject.
    pub maps: Vec<Map>,
}

/// The branches of a plan that share a source and a subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Map {
    /// Its name, unique in the plan, and a path of the GraphAr names it reads: `<type>` for a
    /// vertex type's map, `<type>/<column>` for a column's, `<edge>` (the edge type's
    /// `<source>_<label>_<destination>`) for an edge type's. No name holds a `/`, so the path is
    /// unambiguous, and the mapping writes it as the triples map's fragment, `/` and all.
    pub name: String,
    /// The shape every subject the map produces validates against: its vertex type's, for the
    /// type's own map. No branch: a statement about the map, `dct:conformsTo` in the mapping.
    pub shape: Option<String>,
    /// The relation it reads.
    pub source: Source,
    /// `a_s`.
    pub subject: Expr,
    /// The classes of the subject: a branch each, `a_p` `rdf:type` and `a_o` the class.
    pub classes: Vec<String>,
    /// A branch each: `a_p` the constant predicate, `a_o` the expression.
    pub objects: Vec<(String, Expr)>,
}

/// A source relation, over the tables of a GraphAr graph by their names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A vertex table: an attribute per column.
    Table {
        /// The vertex type.
        table: String,
    },
    /// The elements of a set column, a row each: `subject`, and `value` — or `value` and `lang`
    /// for language-tagged strings.
    Unnest {
        /// The vertex type.
        table: String,
        /// The column.
        column: String,
        /// Whether its elements are language-tagged strings.
        tagged: bool,
    },
    /// A single-valued language-tagged column's two fields: `subject`, `value`, `lang`.
    Fields {
        /// The vertex type.
        table: String,
        /// The column.
        column: String,
    },
    /// An edge table joined to its two end tables on the internal ids: `source` and `target`,
    /// the two subjects.
    Edge {
        /// The edge type's GraphAr name, `{source}_{label}_{destination}`.
        edge: String,
        /// The source vertex type.
        source: String,
        /// The destination vertex type.
        destination: String,
    },
}

/// An extension function, over the attributes of a source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    /// `toIRI(a)`.
    Iri(String),
    /// `toLiteral(a, datatype)`; no datatype is a simple literal, `xsd:string`.
    Literal {
        /// The attribute.
        attribute: String,
        /// The datatype, other than `xsd:string`.
        datatype: Option<String>,
    },
    /// `toLangLiteral(value, lang)`, the extension the module comment names.
    LangLiteral {
        /// The attribute of the lexical form.
        value: String,
        /// The attribute of the language tag.
        lang: String,
    },
}

impl Expr {
    /// The attributes it reads.
    #[must_use]
    pub fn attributes(&self) -> Vec<&str> {
        match self {
            Self::Iri(a) | Self::Literal { attribute: a, .. } => vec![a],
            Self::LangLiteral { value, lang } => vec![value, lang],
        }
    }
}

/// The plan of `schema`: per vertex type a map over its table, with its class, its shape and its
/// single-valued columns (no map when it has none of the three); per set or single
/// language-tagged column a map over its elements or its fields; per edge type a map over its join.
///
/// # Errors
/// What [`Schema::check`] refuses.
pub fn plan(schema: &Schema) -> Result<Plan, Error> {
    schema.check()?;
    let subject = |a: &str| Expr::Iri(a.to_owned());
    let mut maps = Vec::new();
    for v in &schema.vertices {
        let scalars: Vec<(String, Expr)> = v
            .columns
            .iter()
            .filter(|c| c.cardinality == Cardinality::Single && c.storage() != Storage::LangString)
            .map(|c| (c.predicate.clone(), object(&c.object, &c.name)))
            .collect();
        if v.class.is_some() || v.shape.is_some() || !scalars.is_empty() {
            maps.push(Map {
                name: v.label.clone(),
                shape: v.shape.clone(),
                source: Source::Table {
                    table: v.label.clone(),
                },
                subject: subject(SUBJECT),
                classes: v.class.iter().cloned().collect(),
                objects: scalars,
            });
        }
        for c in &v.columns {
            let tagged = c.storage() == Storage::LangString;
            let source = match c.cardinality {
                Cardinality::Set => Source::Unnest {
                    table: v.label.clone(),
                    column: c.name.clone(),
                    tagged,
                },
                Cardinality::Single if tagged => Source::Fields {
                    table: v.label.clone(),
                    column: c.name.clone(),
                },
                Cardinality::Single => continue,
            };
            maps.push(Map {
                name: format!("{}/{}", v.label, c.name),
                shape: None,
                source,
                subject: subject(SUBJECT),
                classes: Vec::new(),
                objects: vec![(c.predicate.clone(), object(&c.object, "value"))],
            });
        }
    }
    for e in &schema.edges {
        maps.push(Map {
            name: e.triple(),
            shape: None,
            source: Source::Edge {
                edge: e.triple(),
                source: e.source.clone(),
                destination: e.destination.clone(),
            },
            subject: subject("source"),
            classes: Vec::new(),
            objects: vec![(e.predicate.clone(), subject("target"))],
        });
    }
    Ok(Plan { maps })
}

/// The expression of a column's values, read from `attribute`.
fn object(kind: &Object, attribute: &str) -> Expr {
    let tagged = || Expr::LangLiteral {
        value: attribute.to_owned(),
        lang: "lang".to_owned(),
    };
    match kind {
        Object::Iri => Expr::Iri(attribute.to_owned()),
        Object::LangString => tagged(),
        Object::Literal(dt) if dt == RDF_LANG_STRING => tagged(),
        // A simple literal is `xsd:string` (RDF 1.1), and a column's natural datatype for text.
        Object::Literal(dt) => Expr::Literal {
            attribute: attribute.to_owned(),
            datatype: (dt != XSD_STRING).then(|| dt.clone()),
        },
    }
}

impl Source {
    /// The relation as SQL:2008: a delimited table name for [`Source::Table`], a query otherwise.
    #[must_use]
    pub fn sql(&self) -> String {
        match self {
            Self::Table { table } => quoted(table),
            Self::Unnest {
                table,
                column,
                tagged: true,
            } => format!(
                "SELECT t.\"{SUBJECT}\", (t.n).\"value\" AS \"value\", (t.n).\"lang\" AS \"lang\" \
                 FROM (SELECT \"{SUBJECT}\", unnest({}) AS n FROM {}) AS t",
                quoted(column),
                quoted(table)
            ),
            Self::Unnest {
                table,
                column,
                tagged: false,
            } => format!(
                "SELECT \"{SUBJECT}\", unnest({}) AS \"value\" FROM {}",
                quoted(column),
                quoted(table)
            ),
            Self::Fields { table, column } => format!(
                "SELECT \"{SUBJECT}\", ({c}).\"value\" AS \"value\", ({c}).\"lang\" AS \"lang\" \
                 FROM {t}",
                c = quoted(column),
                t = quoted(table)
            ),
            Self::Edge {
                edge,
                source,
                destination,
            } => format!(
                "SELECT s.\"{SUBJECT}\" AS \"source\", d.\"{SUBJECT}\" AS \"target\" FROM {edge} AS e \
                 JOIN {src} AS s ON e.\"{SRC}\" = s.\"{VID}\" \
                 JOIN {dst} AS d ON e.\"{DST}\" = d.\"{VID}\"",
                edge = quoted(edge),
                src = quoted(source),
                dst = quoted(destination),
                SRC = graphar_parquet::SRC_INDEX_COL,
                DST = graphar_parquet::DST_INDEX_COL,
                VID = graphar_parquet::VERTEX_INDEX_COL,
            ),
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Iri(a) => write!(f, "toIRI({a})"),
            Self::Literal {
                attribute,
                datatype: None,
            } => write!(f, "toLiteral({attribute}, <{XSD_STRING}>)"),
            Self::Literal {
                attribute,
                datatype: Some(dt),
            } => write!(f, "toLiteral({attribute}, <{dt}>)"),
            Self::LangLiteral { value, lang } => write!(f, "toLangLiteral({value}, {lang})"),
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Source[{}]", self.sql())
    }
}

impl fmt::Display for Map {
    /// Each branch, `π{s,p,o}(ε_o(ε_p(ε_s(Source))))`, on its own line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let classes = self
            .classes
            .iter()
            .map(|c| (RDF_TYPE.to_owned(), format!("<{c}>")));
        let objects = self.objects.iter().map(|(p, o)| (p.clone(), o.to_string()));
        for (i, (p, o)) in classes.chain(objects).enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(
                f,
                "π{{s,p,o}}(ε[o := {o}](ε[p := <{p}>](ε[s := {}]({}))))",
                self.subject, self.source
            )?;
        }
        Ok(())
    }
}

impl fmt::Display for Plan {
    /// The union, a branch per line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "∪(")?;
        for m in &self.maps {
            for line in m.to_string().lines() {
                writeln!(f, "  {line}")?;
            }
        }
        write!(f, ")")
    }
}

/// A name as a SQL delimited identifier.
pub(crate) fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
