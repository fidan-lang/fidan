pub mod async_std;
pub mod collections;
mod common;
pub mod env;
pub mod io;
pub mod math;
pub mod regex;
pub mod string;
pub mod test_runner;
pub mod time;

use crate::FidanValue;

pub fn dispatch_value_module(
    module: &str,
    name: &str,
    args: Vec<FidanValue>,
) -> Option<FidanValue> {
    match module {
        "math" => math::dispatch(name, args),
        "string" => string::dispatch(name, args),
        "io" => io::dispatch(name, args),
        "collections" => collections::dispatch(name, args),
        "env" => env::dispatch(name, args),
        "regex" => regex::dispatch(name, args),
        "time" => time::dispatch(name, args),
        _ => None,
    }
}

pub fn module_exports(module: &str) -> &'static [&'static str] {
    match module {
        "async" => async_std::exported_names(),
        "math" => math::exported_names(),
        "string" => string::exported_names(),
        "io" => io::exported_names(),
        "collections" => collections::exported_names(),
        "env" => env::exported_names(),
        "regex" => regex::exported_names(),
        "time" => time::exported_names(),
        "test" => test_runner::exported_names(),
        _ => &[],
    }
}

pub fn is_stdlib_module(module: &str) -> bool {
    matches!(
        module,
        "async" | "math" | "string" | "io" | "collections" | "env" | "regex" | "time" | "test"
    )
}
