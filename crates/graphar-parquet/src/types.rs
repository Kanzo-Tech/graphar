//! GraphAr's data types as Arrow types: C++ `DataType::DataTypeToArrowDataType` and its inverse
//! (`types.cc`), as a check of the column a writer is handed.

use arrow_schema::{DataType as Arrow, TimeUnit};
use graphar_info::{Cardinality, DataType, Property};

/// Whether `arrow` holds `t`. A string is `Utf8` or `LargeUtf8`, both of which C++ reads as
/// `string`. A `timestamp` is milliseconds, per the specification, in any time zone. `time` has
/// no Arrow type in C++; it is `Time32(Millisecond)`, the specification's "milliseconds since
/// midnight". A user-defined type has none, and matches nothing.
fn holds(t: &DataType, arrow: &Arrow) -> bool {
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
            holds(item, field.data_type())
        }
        _ => false,
    }
}

/// Whether `arrow` holds property `p`: its data type, or a list of it when the property holds
/// several values per vertex.
pub(crate) fn holds_property(p: &Property, arrow: &Arrow) -> bool {
    match p.cardinality {
        Cardinality::Single => holds(&p.data_type, arrow),
        Cardinality::List | Cardinality::Set => match arrow {
            Arrow::List(field) | Arrow::LargeList(field) => holds(&p.data_type, field.data_type()),
            _ => false,
        },
    }
}
