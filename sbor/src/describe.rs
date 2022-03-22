#[cfg(any(feature = "serde_std", feature = "serde_alloc"))]
use serde::{Deserialize, Serialize};

use crate::sbor::{Decode, Encode, TypeId};

use crate::rust::boxed::Box;
use crate::rust::collections::*;
use crate::rust::string::String;
use crate::rust::vec;
use crate::rust::vec::Vec;

// For enum, we use internally tagged representation for readability.
// See: https://serde.rs/enum-representations.html

/// Represents a SBOR type.
#[cfg_attr(
    any(feature = "serde_std", feature = "serde_alloc"),
    derive(Serialize, Deserialize),
    serde(tag = "type")
)]
#[derive(Debug, Clone, PartialEq, Eq, TypeId, Decode, Encode)]
pub enum Type {
    Unit,
    Bool,
    I8,
    I16,
    I32,
    I64,
    I128,
    U8,
    U16,
    U32,
    U64,
    U128,
    String,

    Option {
        value: Box<Type>,
    },

    Array {
        element: Box<Type>,
        length: u16,
    },

    Tuple {
        elements: Vec<Type>,
    },

    Struct {
        name: String,
        fields: Fields,
    },

    Enum {
        name: String,
        variants: Vec<Variant>, // Order matters as it decides of the variant index
    },

    Result {
        okay: Box<Type>,
        error: Box<Type>,
    },

    Vec {
        element: Box<Type>,
    },

    TreeSet {
        element: Box<Type>,
    },

    TreeMap {
        key: Box<Type>,
        value: Box<Type>,
    },

    HashSet {
        element: Box<Type>,
    },

    HashMap {
        key: Box<Type>,
        value: Box<Type>,
    },

    Custom {
        name: String,
        generics: Vec<Type>,
    },
}

/// Represents the type info of an enum variant.
#[cfg_attr(
    any(feature = "serde_std", feature = "serde_alloc"),
    derive(Serialize, Deserialize)
)]
#[derive(Debug, Clone, PartialEq, Eq, TypeId, Decode, Encode)]
pub struct Variant {
    pub name: String,
    pub fields: Fields,
}

/// Represents the type info of struct fields.
#[cfg_attr(
    any(feature = "serde_std", feature = "serde_alloc"),
    derive(Serialize, Deserialize),
    serde(tag = "type")
)]
#[derive(Debug, Clone, PartialEq, Eq, TypeId, Decode, Encode)]
pub enum Fields {
    Named { named: Vec<(String, Type)> },

    Unnamed { unnamed: Vec<Type> },

    Unit,
}

/// A data structure that can be described using SBOR type model.
pub trait Describe {
    fn describe() -> Type;
}

/// Marks a type that does not introduce indirection.
///
/// In Rust, all types are non-recursive and sized at compile time. The only way
/// to support recursion is through indirection (e.g., a `Box`, `Rc`, or `&`).
///
/// This trait is designed to mark the types that do not introduce indirection,
/// thus safe to describe recursively.
pub trait NoIndirection {}

/// A helper method to check if a given type implements the `NoIndirection`
/// trait at compile time.
pub fn require_no_indirection<T: NoIndirection>() {}

impl Describe for () {
    fn describe() -> Type {
        Type::Unit
    }
}
impl NoIndirection for () {}

macro_rules! describe_basic_type {
    ($type:ident, $type_id:expr) => {
        impl Describe for $type {
            fn describe() -> Type {
                $type_id
            }
        }
        impl NoIndirection for $type {}
    };
}

describe_basic_type!(bool, Type::Bool);
describe_basic_type!(i8, Type::I8);
describe_basic_type!(i16, Type::I16);
describe_basic_type!(i32, Type::I32);
describe_basic_type!(i64, Type::I64);
describe_basic_type!(i128, Type::I128);
describe_basic_type!(u8, Type::U8);
describe_basic_type!(u16, Type::U16);
describe_basic_type!(u32, Type::U32);
describe_basic_type!(u64, Type::U64);
describe_basic_type!(u128, Type::U128);
describe_basic_type!(isize, Type::I32);
describe_basic_type!(usize, Type::U32);
describe_basic_type!(str, Type::String);
describe_basic_type!(String, Type::String);

impl<T: Describe> Describe for Option<T> {
    fn describe() -> Type {
        let ty = T::describe();
        Type::Option {
            value: Box::new(ty),
        }
    }
}
impl<T: NoIndirection> NoIndirection for Option<T> {}

impl<T: Describe, const N: usize> Describe for [T; N] {
    fn describe() -> Type {
        let ty = T::describe();
        Type::Array {
            element: Box::new(ty),
            length: N as u16,
        }
    }
}
impl<T: NoIndirection, const N: usize> NoIndirection for [T; N] {}

macro_rules! describe_tuple {
    ($($name:ident)+) => {
        impl<$($name: Describe),+> Describe for ($($name,)+) {
            fn describe() -> Type {
                Type::Tuple { elements: vec![ $($name::describe(),)* ] }
            }
        }
        impl<$($name: NoIndirection),+> NoIndirection for ($($name,)+) {}
    };
}

describe_tuple! { A B }
describe_tuple! { A B C }
describe_tuple! { A B C D }
describe_tuple! { A B C D E }
describe_tuple! { A B C D E F }
describe_tuple! { A B C D E F G }
describe_tuple! { A B C D E F G H }
describe_tuple! { A B C D E F G H I }
describe_tuple! { A B C D E F G H I J }

impl<T: Describe, E: Describe> Describe for Result<T, E> {
    fn describe() -> Type {
        let t = T::describe();
        let e = E::describe();
        Type::Result {
            okay: Box::new(t),
            error: Box::new(e),
        }
    }
}
impl<T: NoIndirection, E: NoIndirection> NoIndirection for Result<T, E> {}

impl<T: Describe> Describe for Vec<T> {
    fn describe() -> Type {
        let ty = T::describe();
        Type::Vec {
            element: Box::new(ty),
        }
    }
}
// Vec<T> introduces indirection

impl<T: Describe> Describe for BTreeSet<T> {
    fn describe() -> Type {
        let ty = T::describe();
        Type::TreeSet {
            element: Box::new(ty),
        }
    }
}
// BTreeSet<T> introduces indirection

impl<K: Describe, V: Describe> Describe for BTreeMap<K, V> {
    fn describe() -> Type {
        let k = K::describe();
        let v = V::describe();
        Type::TreeMap {
            key: Box::new(k),
            value: Box::new(v),
        }
    }
}
// BTreeMap<K, V> introduces indirection

impl<T: Describe> Describe for HashSet<T> {
    fn describe() -> Type {
        let ty = T::describe();
        Type::HashSet {
            element: Box::new(ty),
        }
    }
}
// HashSet<T> introduces indirection

impl<K: Describe, V: Describe> Describe for HashMap<K, V> {
    fn describe() -> Type {
        let k = K::describe();
        let v = V::describe();
        Type::HashMap {
            key: Box::new(k),
            value: Box::new(v),
        }
    }
}
// HashMap<K, V> introduces indirection

#[cfg(test)]
mod tests {
    use crate::describe::*;
    use crate::rust::boxed::Box;
    use crate::rust::string::String;
    use crate::rust::vec;

    #[test]
    pub fn test_basic_types() {
        assert_eq!(Type::Bool, bool::describe());
        assert_eq!(Type::I8, i8::describe());
        assert_eq!(Type::I16, i16::describe());
        assert_eq!(Type::I32, i32::describe());
        assert_eq!(Type::I64, i64::describe());
        assert_eq!(Type::I128, i128::describe());
        assert_eq!(Type::U8, u8::describe());
        assert_eq!(Type::U16, u16::describe());
        assert_eq!(Type::U32, u32::describe());
        assert_eq!(Type::U64, u64::describe());
        assert_eq!(Type::U128, u128::describe());
        assert_eq!(Type::String, String::describe());
    }

    #[test]
    pub fn test_option() {
        assert_eq!(
            Type::Option {
                value: Box::new(Type::String)
            },
            Option::<String>::describe(),
        );
    }

    #[test]
    pub fn test_array() {
        assert_eq!(
            Type::Array {
                element: Box::new(Type::U8),
                length: 3,
            },
            <[u8; 3]>::describe(),
        );
    }

    #[test]
    pub fn test_tuple() {
        assert_eq!(
            Type::Tuple {
                elements: vec![Type::U8, Type::U128]
            },
            <(u8, u128)>::describe(),
        );
    }

    #[test]
    pub fn test_option_of_vec() {
        assert_eq!(
            Type::Option {
                value: Box::new(Type::Vec {
                    element: Box::new(Type::String)
                })
            },
            <Option<Vec<String>>>::describe(),
        );
    }

    #[test]
    pub fn test_vec() {
        assert_eq!(
            Type::Vec {
                element: Box::new(Type::String)
            },
            <Vec<String>>::describe(),
        );
    }
}
