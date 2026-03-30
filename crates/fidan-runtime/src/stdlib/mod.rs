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

#[derive(Clone, Copy)]
struct ValueModuleInfo {
    name: &'static str,
    dispatch: fn(&str, Vec<FidanValue>) -> Option<FidanValue>,
    exports: fn() -> &'static [&'static str],
}

const VALUE_MODULES: &[ValueModuleInfo] = &[
    ValueModuleInfo {
        name: "math",
        dispatch: math::dispatch,
        exports: math::exported_names,
    },
    ValueModuleInfo {
        name: "string",
        dispatch: string::dispatch,
        exports: string::exported_names,
    },
    ValueModuleInfo {
        name: "io",
        dispatch: io::dispatch,
        exports: io::exported_names,
    },
    ValueModuleInfo {
        name: "collections",
        dispatch: collections::dispatch,
        exports: collections::exported_names,
    },
    ValueModuleInfo {
        name: "env",
        dispatch: env::dispatch,
        exports: env::exported_names,
    },
    ValueModuleInfo {
        name: "regex",
        dispatch: regex::dispatch,
        exports: regex::exported_names,
    },
    ValueModuleInfo {
        name: "time",
        dispatch: time::dispatch,
        exports: time::exported_names,
    },
];

fn value_module(module: &str) -> Option<ValueModuleInfo> {
    VALUE_MODULES
        .iter()
        .copied()
        .find(|info| info.name == module)
}

pub fn dispatch_value_module(
    module: &str,
    name: &str,
    args: Vec<FidanValue>,
) -> Option<FidanValue> {
    let info = value_module(module)?;
    (info.dispatch)(name, args)
}

pub fn module_exports(module: &str) -> &'static [&'static str] {
    match module {
        "async" => async_std::exported_names(),
        "test" => test_runner::exported_names(),
        _ => value_module(module)
            .map(|info| (info.exports)())
            .unwrap_or(&[]),
    }
}

pub fn is_stdlib_module(module: &str) -> bool {
    matches!(module, "async" | "test") || value_module(module).is_some()
}
