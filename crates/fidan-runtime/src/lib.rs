//! `fidan-runtime` — Value types, memory model (OwnedRef/SharedRef/COW), object model.

mod value;
mod owned_ref;
mod shared_ref;
mod object;
mod string;
mod list;
mod dict;

pub use value::{FidanValue, FunctionId};
pub use owned_ref::OwnedRef;
pub use shared_ref::SharedRef;
pub use object::{FidanObject, FidanClass, FieldDef};
pub use string::FidanString;
pub use list::FidanList;
pub use dict::FidanDict;
