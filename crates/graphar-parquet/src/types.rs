//! GraphAr's data types as Arrow types: C++ `DataType::DataTypeToArrowDataType` and its inverse
//! (`types.cc`), as a check of the column a writer is handed.

use std::collections::BTreeMap;

use arrow_schema::{DataType as Arrow, TimeUnit};
use graphar_info::{Cardinality, DataType, Property};
use parquet::file::properties::WriterProperties;

/// The Arrow type of each user-defined type a graph's version declares (`gar/v1 (langString)`),
/// by name. GraphAr declares user-defined types and says nothing of how they are stored, so the
/// writer's caller says: a column of a property typed `langString` must have exactly the Arrow
/// type under `langString`, and a name with no entry has no column that can hold it. Nothing in
/// this crate knows any name.
pub type UserTypes = BTreeMap<String, Arrow>;

/// How the writers write: Parquet's [`WriterProperties`] (the codec: a feature of `parquet` the
/// caller turns on) and the Arrow type of each user-defined type.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Parquet's own writer settings, as `ParquetObjectWriter` takes them.
    pub props: WriterProperties,
    /// The Arrow type of each user-defined type the graph declares.
    pub user_types: UserTypes,
}

/// Whether `arrow` holds `t`. The specification ("Property Data Types") fixes the meaning of each
/// type and not its Arrow type; where it is silent this is C++'s `DataTypeToArrowDataType`
/// (`types.cc`, v0.13.0), and every departure is named here:
///
/// - `string` ("Textual data") is `Utf8` or `LargeUtf8`; C++ writes `LargeUtf8` and reads both.
/// - `timestamp` ("milliseconds since the Unix epoch") is a millisecond `Timestamp` in any time
///   zone; C++ writes it without one. The specification does not mention a zone.
/// - `time` ("milliseconds since midnight") is `Time32(Millisecond)`. **A departure from C++**,
///   which has no Arrow type for it and refuses the name (`TypeNameToDataType`); the specification
///   lists `time`, and the specification wins.
/// - `list<T>` ("A list of values of the same type") is a `List` or `LargeList` of any `T` held,
///   nested as deep as the document says. A property whose cardinality is not `single` is one
///   level of list of its type, as C++ writes it (`VertexPropertyWriter::validate`). **A departure from C++**, whose `TypeNameToDataType`
///   accepts five element types only.
/// - A user-defined type ("GraphAr also supports the user-defined data types, which can be used to
///   represent complex data structures, such as the struct, map, and union types") is held by the
///   Arrow type `user` gives its name, exactly. The specification defines no mapping, so this is
///   this crate's extension, declared by [`UserTypes`] rather than assumed. C++ refuses a graph
///   with one (`TypeNameToDataType` throws on any name it does not list), whatever its version
///   declares.
fn holds(t: &DataType, arrow: &Arrow, user: &UserTypes) -> bool {
    match (t, arrow) {
        (DataType::Bool, Arrow::Boolean)
        | (DataType::Int32, Arrow::Int32)
        | (DataType::Int64, Arrow::Int64)
        | (DataType::Float, Arrow::Float32)
        | (DataType::Double, Arrow::Float64)
        | (DataType::String, Arrow::Utf8 | Arrow::LargeUtf8)
        | (DataType::Date, Arrow::Date32)
        | (DataType::Timestamp, Arrow::Timestamp(TimeUnit::Millisecond, _))
        | (DataType::Time, Arrow::Time32(TimeUnit::Millisecond)) => true,
        (DataType::List(item), Arrow::List(field) | Arrow::LargeList(field)) => {
            holds(item, field.data_type(), user)
        }
        (DataType::UserDefined(name), _) => user.get(name) == Some(arrow),
        _ => false,
    }
}

/// Whether `arrow` holds property `p`: its data type, or a list of it when the property holds
/// several values per vertex.
pub(crate) fn holds_property(p: &Property, arrow: &Arrow, user: &UserTypes) -> bool {
    match p.cardinality {
        Cardinality::Single => holds(&p.data_type, arrow, user),
        Cardinality::List | Cardinality::Set => match arrow {
            Arrow::List(field) | Arrow::LargeList(field) => {
                holds(&p.data_type, field.data_type(), user)
            }
            _ => false,
        },
    }
}
