//! `fidan-runtime` — Value types, memory model (OwnedRef/SharedRef/COW), object model.

mod dict;
mod list;
mod object;
mod owned_ref;
pub mod parallel;
mod shared_ref;
mod string;
mod value;

pub use dict::FidanDict;
pub use list::FidanList;
pub use object::{FidanClass, FidanObject, FieldDef};
pub use owned_ref::OwnedRef;
pub use parallel::{FidanPending, ParallelArgs, ParallelCapture};
pub use shared_ref::SharedRef;
pub use string::FidanString;
pub use value::{FidanValue, FunctionId, display};
