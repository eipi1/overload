//! ## Purpose
//! Support for external data. Now only support file as data source. Maybe that's the only thing I
//! will ever need.
//!
//!
//! ## Requirements
//! * Data source, will be referred as source moving forward, needs to be in tabular format
//! * The resulting data type is a JSON object
//! * Object can have reference to only one row unless the source is referred from an array
//! * An array can have reference to multiple rows of a source
//! * Array elements can be either objects or string or primitive types
//! * An object can have only single array referencing to the same source[^1]
//!
//! [^1]: The size of two arrays can be different. If multiple array refer to same source, it's difficult to define
//! how many rows to fetch from the source.
//!

use derivative::Derivative;

#[derive(Derivative, Clone, Debug)]
#[derivative(PartialEq, Hash)]
pub enum ExternalData {
    Array(String, Vec<Self>),
    Object {
        path: String,
        #[derivative(PartialEq = "ignore")]
        #[derivative(Hash = "ignore")]
        count: usize,
    },
}

impl ExternalData {
    pub(crate) fn set_object_count(&mut self, new_count: usize) {
        if let ExternalData::Object { path: _, count } = self {
            *count = new_count;
        }
    }
}

impl From<&ExternalData> for ExternalData {
    fn from(value: &ExternalData) -> Self {
        value.clone()
    }
}

// impl PartialEq for ExternalData {
//     fn eq(&self, other: &Self) -> bool {
//         let self_tag = std::mem::discriminant(self);
//         let other_tag = std::mem::discriminant(other);
//         self_tag == other_tag &&
//             match (self, other) {
//                 (
//                     ExternalData::Array(__self_0, __self_1),
//                     ExternalData::Array(__arg1_0, __arg1_1),
//                 ) => *__self_0 == *__arg1_0 && *__self_1 == *__arg1_1,
//                 (
//                     ExternalData::Object { path: __self_0, count: __self_1 },
//                     ExternalData::Object { path: __arg1_0, count: __arg1_1 },
//                 ) => *__self_0 == *__arg1_0,
//                 _ => false
//             }
//     }
// }

impl Eq for ExternalData {}

// struct ExternalDataDescriptor {
//     path: String,
//     data_type: Type,
//     child: Vec<ExternalDataDescriptor>
// }
