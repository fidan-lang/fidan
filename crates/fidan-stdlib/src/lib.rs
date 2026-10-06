//! `fidan-stdlib` — Standard library implementations (Rust, callable from Fidan via FFI).
//!
//! # Import system
//!
//! Fidan's `use` statement resolves stdlib paths at interpreter startup:
//!
//! ```fidan
//! use std.io              # registers io.* as module namespace
//! use std.io.{readFile}   # injects readFile as a free builtin
//! use std.math            # registers math.* as module namespace
//! ```
//!
//! The `StdlibRegistry` maps fully-qualified paths (e.g. `"std.math"`) to
//! `StdlibModule` descriptors. The MIR interpreter queries the registry when
//! it resolves `Callee::StdlibFn { module, name }` calls.

pub mod async_std;
pub mod collections;
pub mod io;
pub mod math;
pub mod metadata;
pub mod parallel;
pub mod regex;
pub mod sandbox;
pub mod string;
pub mod test_runner;
pub mod time;

use fidan_diagnostics::DiagCode;

pub use fidan_config::stdlib::{StdlibMemberInfo, member_info, module_members};

/// A dispatched stdlib call result.
pub use sandbox::{SandboxPolicy, SandboxViolation};

#[derive(Clone, Copy)]
pub struct StdlibModuleInfo {
    pub name: &'static str,
    pub exports: fn() -> &'static [&'static str],
    pub doc: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdlibParamInfo {
    pub name: String,
    pub type_name: &'static str,
    pub optional: bool,
    pub variadic: bool,
}

pub const STDLIB_MODULES: &[StdlibModuleInfo] = &[
    StdlibModuleInfo {
        name: "async",
        exports: fidan_runtime::stdlib::async_std::exported_names,
        doc: "Same-thread async helpers like sleep, gather, waitAny, and timeout.",
    },
    StdlibModuleInfo {
        name: "collections",
        exports: fidan_runtime::stdlib::collections::exported_names,
        doc: "Collection helpers like zip, enumerate, chunk, window, partition, and groupBy.",
    },
    StdlibModuleInfo {
        name: "env",
        exports: fidan_runtime::stdlib::env::exported_names,
        doc: "Environment variables and process arguments.",
    },
    StdlibModuleInfo {
        name: "io",
        exports: fidan_runtime::stdlib::io::exported_names,
        doc: "Printing, input, file I/O, paths, directories, and terminal helpers.",
    },
    StdlibModuleInfo {
        name: "json",
        exports: fidan_runtime::stdlib::json::exported_names,
        doc: "JSON parsing, validation, compact serialization, and pretty-print helpers.",
    },
    StdlibModuleInfo {
        name: "math",
        exports: fidan_runtime::stdlib::math::exported_names,
        doc: "Math functions, constants, random helpers, and numeric transforms.",
    },
    StdlibModuleInfo {
        name: "parallel",
        exports: parallel::exported_names,
        doc: "Thread-backed parallel collection helpers.",
    },
    StdlibModuleInfo {
        name: "regex",
        exports: fidan_runtime::stdlib::regex::exported_names,
        doc: "Regex compile, match, capture, replace, and split helpers.",
    },
    StdlibModuleInfo {
        name: "string",
        exports: fidan_runtime::stdlib::string::exported_names,
        doc: "String transforms, parsing, slicing, casing, and character helpers.",
    },
    StdlibModuleInfo {
        name: "test",
        exports: test_runner::exported_names,
        doc: "Assertion helpers used by `fidan test` and inline test blocks.",
    },
    StdlibModuleInfo {
        name: "time",
        exports: fidan_runtime::stdlib::time::exported_names,
        doc: "Clocks, elapsed timing, sleep/wait, and date/time helpers.",
    },
];

pub fn module_info(module: &str) -> Option<&'static StdlibModuleInfo> {
    STDLIB_MODULES.iter().find(|info| info.name == module)
}

fn canonical_member_name(module: &str, name: &str) -> Option<&'static str> {
    member_info(module, name).and_then(|info| info.names.first().copied())
}

fn inferred_param_type_name(
    module: &str,
    canonical_name: &str,
    index: usize,
    param_name: &str,
) -> &'static str {
    match module {
        "async" => match canonical_name {
            "sleep" => "integer",
            "ready" => "dynamic",
            "gather" | "waitAny" => "list oftype Pending oftype dynamic",
            "timeout" => {
                if index == 0 {
                    "Pending oftype dynamic"
                } else {
                    "integer"
                }
            }
            _ => "dynamic",
        },
        "collections" => match canonical_name {
            "range" => "integer",
            "hashset" => "dynamic",
            "setAdd" | "setRemove" | "setContains" if index == 0 => "hashset oftype dynamic",
            "setToList" | "setLen" => "hashset oftype dynamic",
            "setUnion" | "setIntersect" | "setDiff" => "hashset oftype dynamic",
            "Queue" | "Stack" => "list oftype dynamic",
            "enqueue" | "push" if index == 0 => "list oftype dynamic",
            "dequeue" | "peek" | "pop" | "top" => "list oftype dynamic",
            "flatten" | "zip" | "enumerate" | "partition" | "groupBy" | "unique" | "reverse"
            | "sort" | "len" | "isEmpty" | "slice" | "first" | "last" | "sum" | "product"
            | "min" | "max" => {
                if param_name == "size" || param_name == "start" || param_name == "end" {
                    "integer"
                } else if param_name == "separator" {
                    "string"
                } else {
                    "list oftype dynamic"
                }
            }
            "chunk" | "window" => {
                if index == 0 {
                    "list oftype dynamic"
                } else {
                    "integer"
                }
            }
            "concat" => "list oftype dynamic",
            "join" => {
                if index == 0 {
                    "list oftype dynamic"
                } else {
                    "string"
                }
            }
            _ => "dynamic",
        },
        "env" => match canonical_name {
            "get" | "set" => "string",
            _ => "dynamic",
        },
        "io" => match canonical_name {
            "print" | "eprint" => "dynamic",
            "readLine" => "string",
            "readFile" | "readLines" | "deleteFile" | "fileExists" | "isFile" | "isDir"
            | "makeDir" | "listDir" | "dirname" | "basename" | "extension" | "absolutePath"
            | "getEnv" => "string",
            "writeFile" | "appendFile" | "setEnv" => "string",
            "copyFile" | "renameFile" => "string",
            "join" | "joinPath" => "string",
            "isatty" => "string",
            _ => "dynamic",
        },
        "json" => match canonical_name {
            "loads" | "parse" | "isValid" => {
                if index == 0 {
                    "string"
                } else {
                    "boolean"
                }
            }
            "load" => {
                if index == 0 {
                    "string"
                } else {
                    "boolean"
                }
            }
            "dump" if index == 1 => "string",
            "dumps" | "stringify" | "pretty" => "dynamic",
            _ => "dynamic",
        },
        "math" => match canonical_name {
            "pi" | "e" | "tau" | "inf" | "nan" | "random" => "dynamic",
            "randomInt" => "integer",
            _ => "float",
        },
        "parallel" => match canonical_name {
            "parallelReduce" => match index {
                0 => "list oftype dynamic",
                1 => "dynamic",
                _ => "action",
            },
            "parallelMap" | "parallelFilter" | "parallelForEach" => {
                if index == 0 {
                    "list oftype dynamic"
                } else {
                    "action"
                }
            }
            _ => "dynamic",
        },
        "regex" => "string",
        "string" => match canonical_name {
            "join" => {
                if index == 0 {
                    "string"
                } else {
                    "list oftype dynamic"
                }
            }
            "slice" => {
                if index == 0 {
                    "string"
                } else {
                    "integer"
                }
            }
            "padStart" | "padEnd" => match index {
                0 => "string",
                1 => "integer",
                _ => "string",
            },
            "repeat" => {
                if index == 0 {
                    "string"
                } else {
                    "integer"
                }
            }
            "format" => {
                if index == 0 {
                    "string"
                } else {
                    "dynamic"
                }
            }
            "fromChars" => "list oftype dynamic",
            "fromCharCode" => "integer",
            _ => match param_name {
                "text" | "separator" | "pattern" | "prefix" | "suffix" | "from" | "to"
                | "template" | "pad" => "string",
                "width" | "n" | "code" => "integer",
                "list" => "list oftype dynamic",
                "chars" => "list oftype dynamic",
                _ => "dynamic",
            },
        },
        "test" => match canonical_name {
            "assert" => {
                if index == 0 {
                    "boolean"
                } else {
                    "string"
                }
            }
            "assertType" => match index {
                0 => "dynamic",
                1 => "string",
                _ => "string",
            },
            "fail" | "skip" => "string",
            _ => {
                if index < 2 {
                    "dynamic"
                } else {
                    "string"
                }
            }
        },
        "time" => match canonical_name {
            "now" | "timestamp" => "dynamic",
            "sleep" | "elapsed" | "date" | "time" | "datetime" | "year" | "month" | "day"
            | "hour" | "minute" | "second" | "weekday" => "integer",
            "format" => {
                if index == 0 {
                    "integer"
                } else {
                    "string"
                }
            }
            _ => "dynamic",
        },
        _ => "dynamic",
    }
}

pub fn member_params(module: &str, name: &str) -> Option<Vec<StdlibParamInfo>> {
    let canonical_name = canonical_member_name(module, name)?;
    let params = fidan_config::stdlib::member_call_params(module, name)?;

    Some(
        params
            .into_iter()
            .enumerate()
            .map(|(index, param)| StdlibParamInfo {
                type_name: inferred_param_type_name(module, canonical_name, index, &param.name),
                name: param.name,
                optional: param.optional,
                variadic: param.variadic,
            })
            .collect(),
    )
}

pub fn member_signature_with_return(
    module: &str,
    name: &str,
    return_type: Option<&str>,
) -> Option<String> {
    let info = member_info(module, name)?;
    let open = info.signature.find('(')?;
    let close = info.signature.rfind(')')?;
    let params = member_params(module, name)?
        .into_iter()
        .map(|param| {
            let mut rendered = format!("{} oftype {}", param.name, param.type_name);
            if param.variadic {
                rendered.push_str("...");
            }
            if param.optional {
                rendered.push('?');
            }
            rendered
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut signature = format!("{}({params})", &info.signature[..open],);
    if params.is_empty() {
        signature = format!("{}()", &info.signature[..open]);
    }
    if let Some(ret_type) = return_type {
        signature.push_str(" -> ");
        signature.push_str(ret_type);
    }
    let _ = close;
    Some(signature)
}

pub fn member_signature(module: &str, name: &str) -> Option<String> {
    member_signature_with_return(module, name, member_return_type(module, name))
}

pub fn member_doc(module: &str, name: &str) -> Option<String> {
    let info = member_info(module, name)?;
    let signature = member_signature(module, name).unwrap_or_else(|| info.signature.to_string());
    Some(format!("```fidan\n{}\n```\n\n{}", signature, info.doc))
}

/// Returns the canonical static return-type metadata for a stdlib member.
///
/// ```rust
/// assert_eq!(fidan_stdlib::member_return_type("collections", "zip"), Some("list oftype (dynamic, dynamic)"));
/// assert_eq!(fidan_stdlib::member_return_type("async", "waitAny"), Some("Pending oftype (integer, dynamic)"));
/// ```
pub fn member_return_type(module: &str, name: &str) -> Option<&'static str> {
    let info = member_info(module, name)?;
    Some(match info.signature {
        // async
        "std.async.sleep(ms)" => "Pending oftype nothing",
        "std.async.ready(value)" => "Pending oftype dynamic",
        "std.async.gather(handles)" => "Pending oftype list oftype dynamic",
        "std.async.waitAny(handles)" => "Pending oftype (integer, dynamic)",
        "std.async.timeout(handle, ms)" => "Pending oftype (boolean, dynamic)",

        // collections
        "std.collections.range(start, stop, step?)" => "list oftype integer",
        "std.collections.hashset(items?)" => "hashset oftype dynamic",
        "std.collections.setAdd(set, value)" => "nothing",
        "std.collections.setRemove(set, value)" => "nothing",
        "std.collections.setContains(set, value)" => "boolean",
        "std.collections.setToList(set)" => "list oftype dynamic",
        "std.collections.setLen(set)" => "integer",
        "std.collections.setUnion(left, right)" => "hashset oftype dynamic",
        "std.collections.setIntersect(left, right)" => "hashset oftype dynamic",
        "std.collections.setDiff(left, right)" => "hashset oftype dynamic",
        "std.collections.Queue(items?)" => "list oftype dynamic",
        "std.collections.enqueue(queue, value)" => "nothing",
        "std.collections.dequeue(queue)" => "dynamic",
        "std.collections.peek(queue)" => "dynamic",
        "std.collections.Stack(items?)" => "list oftype dynamic",
        "std.collections.push(stack, value)" => "nothing",
        "std.collections.pop(stack)" => "dynamic",
        "std.collections.top(stack)" => "dynamic",
        "std.collections.flatten(list)" => "list oftype dynamic",
        "std.collections.zip(left, right)" => "list oftype (dynamic, dynamic)",
        "std.collections.enumerate(list)" => "list oftype (integer, dynamic)",
        "std.collections.chunk(list, size)" => "list oftype list oftype dynamic",
        "std.collections.window(list, size)" => "list oftype list oftype dynamic",
        "std.collections.partition(list)" => "(list oftype dynamic, list oftype dynamic)",
        "std.collections.groupBy(list)" => "dict oftype (dynamic, list oftype dynamic)",
        "std.collections.unique(list)" => "list oftype dynamic",
        "std.collections.reverse(list)" => "list oftype dynamic",
        "std.collections.sort(list)" => "list oftype dynamic",
        "std.collections.len(list)" => "integer",
        "std.collections.isEmpty(list)" => "boolean",
        "std.collections.concat(items...)" => "list oftype dynamic",
        "std.collections.slice(list, start, end?)" => "list oftype dynamic",
        "std.collections.first(list)" => "dynamic",
        "std.collections.last(list)" => "dynamic",
        "std.collections.join(list, separator)" => "string",
        "std.collections.sum(list)" => "dynamic",
        "std.collections.product(list)" => "dynamic",
        "std.collections.min(list)" => "dynamic",
        "std.collections.max(list)" => "dynamic",

        // env
        "std.env.get(key)" => "string",
        "std.env.set(key, value)" => "nothing",
        "std.env.args()" => "list oftype string",

        // io
        "std.io.print(value...)" => "nothing",
        "std.io.eprint(value...)" => "nothing",
        "std.io.readLine(prompt?)" => "string",
        "std.io.readFile(path)" => "string",
        "std.io.readLines(path)" => "list oftype string",
        "std.io.writeFile(path, content)" => "boolean",
        "std.io.appendFile(path, content)" => "boolean",
        "std.io.deleteFile(path)" => "boolean",
        "std.io.fileExists(path)" => "boolean",
        "std.io.isFile(path)" => "boolean",
        "std.io.isDir(path)" => "boolean",
        "std.io.makeDir(path)" => "boolean",
        "std.io.listDir(path)" => "list oftype string",
        "std.io.copyFile(from, to)" => "boolean",
        "std.io.renameFile(from, to)" => "boolean",
        "std.io.joinPath(part...)" => "string",
        "std.io.dirname(path)" => "string",
        "std.io.basename(path)" => "string",
        "std.io.extension(path)" => "string",
        "std.io.cwd()" => "string",
        "std.io.absolutePath(path)" => "string",
        "std.io.getEnv(key)" => "string",
        "std.io.setEnv(key, value)" => "nothing",
        "std.io.args()" => "list oftype string",
        "std.io.flush()" => "nothing",
        "std.io.isatty(stream?)" => "boolean",

        // json
        "std.json.loads(text, soft?)" => "dynamic",
        "std.json.parse(text, soft?)" => "dynamic",
        "std.json.load(path, soft?)" => "dynamic",
        "std.json.dumps(value)" => "string",
        "std.json.stringify(value)" => "string",
        "std.json.dump(value, path)" => "boolean",
        "std.json.pretty(value)" => "string",
        "std.json.isValid(text)" => "boolean",

        // math
        "std.math.sin(x)" => "float",
        "std.math.cos(x)" => "float",
        "std.math.tan(x)" => "float",
        "std.math.asin(x)" => "float",
        "std.math.acos(x)" => "float",
        "std.math.atan(x)" => "float",
        "std.math.atan2(y, x)" => "float",
        "std.math.sinh(x)" => "float",
        "std.math.cosh(x)" => "float",
        "std.math.tanh(x)" => "float",
        "std.math.sqrt(x)" => "float",
        "std.math.cbrt(x)" => "float",
        "std.math.pow(x, y)" => "float",
        "std.math.exp(x)" => "float",
        "std.math.exp2(x)" => "float",
        "std.math.log(x)" => "float",
        "std.math.log2(x)" => "float",
        "std.math.log10(x)" => "float",
        "std.math.logN(x, base)" => "float",
        "std.math.floor(x)" => "integer",
        "std.math.ceil(x)" => "integer",
        "std.math.round(x)" => "integer",
        "std.math.trunc(x)" => "float",
        "std.math.fract(x)" => "float",
        "std.math.abs(x)" => "dynamic",
        "std.math.sign(x)" => "dynamic",
        "std.math.min(a, b)" => "dynamic",
        "std.math.max(a, b)" => "dynamic",
        "std.math.clamp(x, lo, hi)" => "float",
        "std.math.hypot(x, y)" => "float",
        "std.math.pi()" => "float",
        "std.math.e()" => "float",
        "std.math.tau()" => "float",
        "std.math.inf()" => "float",
        "std.math.nan()" => "float",
        "std.math.isNan(x)" => "boolean",
        "std.math.isInfinite(x)" => "boolean",
        "std.math.isFinite(x)" => "boolean",
        "std.math.random()" => "float",
        "std.math.randomInt(lo, hi)" => "integer",
        "std.math.toDeg(x)" => "float",
        "std.math.toRad(x)" => "float",

        // parallel
        "std.parallel.parallelMap(list, fn)" => "list oftype dynamic",
        "std.parallel.parallelFilter(list, fn)" => "list oftype dynamic",
        "std.parallel.parallelForEach(list, fn)" => "nothing",
        "std.parallel.parallelReduce(list, init, fn)" => "dynamic",

        // regex
        "std.regex.test(pattern, subject)" => "boolean",
        "std.regex.match(pattern, subject)" => "string",
        "std.regex.findAll(pattern, subject)" => "list oftype string",
        "std.regex.capture(pattern, subject)" => "list oftype dynamic",
        "std.regex.captureAll(pattern, subject)" => "list oftype list oftype dynamic",
        "std.regex.replace(pattern, subject, replacement)" => "string",
        "std.regex.replaceAll(pattern, subject, replacement)" => "string",
        "std.regex.split(pattern, subject)" => "list oftype string",
        "std.regex.isValid(pattern)" => "boolean",

        // string
        "std.string.toUpper(text)" => "string",
        "std.string.toLower(text)" => "string",
        "std.string.capitalize(text)" => "string",
        "std.string.trim(text)" => "string",
        "std.string.trimStart(text)" => "string",
        "std.string.trimEnd(text)" => "string",
        "std.string.split(text, separator)" => "list oftype string",
        "std.string.join(separator, list)" => "string",
        "std.string.lines(text)" => "list oftype string",
        "std.string.contains(text, pattern)" => "boolean",
        "std.string.startsWith(text, prefix)" => "boolean",
        "std.string.endsWith(text, suffix)" => "boolean",
        "std.string.indexOf(text, pattern)" => "integer",
        "std.string.lastIndexOf(text, pattern)" => "integer",
        "std.string.replace(text, from, to)" => "string",
        "std.string.replaceFirst(text, from, to)" => "string",
        "std.string.slice(text, start, end?)" => "string",
        "std.string.padStart(text, width, pad?)" => "string",
        "std.string.padEnd(text, width, pad?)" => "string",
        "std.string.repeat(text, n)" => "string",
        "std.string.reverse(text)" => "string",
        "std.string.len(text)" => "integer",
        "std.string.isEmpty(text)" => "boolean",
        "std.string.format(template, value...)" => "string",
        "std.string.parseInt(text)" => "integer",
        "std.string.parseFloat(text)" => "float",
        "std.string.chars(text)" => "list oftype string",
        "std.string.bytes(text)" => "list oftype integer",
        "std.string.fromChars(chars)" => "string",
        "std.string.charCode(text)" => "integer",
        "std.string.fromCharCode(code)" => "string",

        // test
        "std.test.assert(condition, message?)" => "nothing",
        "std.test.assertEq(left, right, message?)" => "nothing",
        "std.test.assertNe(left, right, message?)" => "nothing",
        "std.test.assertGt(left, right, message?)" => "nothing",
        "std.test.assertLt(left, right, message?)" => "nothing",
        "std.test.assertSome(value, message?)" => "nothing",
        "std.test.assertNothing(value, message?)" => "nothing",
        "std.test.assertType(value, typeName, message?)" => "nothing",
        "std.test.fail(message?)" => "nothing",
        "std.test.skip(message?)" => "nothing",

        // time
        "std.time.now()" => "integer",
        "std.time.timestamp()" => "integer",
        "std.time.sleep(ms)" => "nothing",
        "std.time.elapsed(startMs)" => "integer",
        "std.time.date(ms?)" => "string",
        "std.time.time(ms?)" => "string",
        "std.time.datetime(ms?)" => "string",
        "std.time.format(ms?, pattern)" => "string",
        "std.time.year(ms?)" => "integer",
        "std.time.month(ms?)" => "integer",
        "std.time.day(ms?)" => "integer",
        "std.time.hour(ms?)" => "integer",
        "std.time.minute(ms?)" => "integer",
        "std.time.second(ms?)" => "integer",
        "std.time.weekday(ms?)" => "integer",
        _ => return None,
    })
}

/// A dispatched stdlib call result.
pub enum StdlibResult {
    /// Synchronous result value.
    Value(fidan_runtime::FidanValue),
    /// The call requires async/pending orchestration in the host runtime.
    NeedsAsyncDispatch(async_std::AsyncOp),
    /// The call requires callback dispatch (e.g. parallelMap needs MIR fn dispatch).
    /// Contains an opaque bytes payload — the parallel module's `ParallelOp`.
    NeedsCallbackDispatch(parallel::ParallelOp),
}

pub struct StdlibError {
    pub code: DiagCode,
    pub message: String,
}

/// Dispatch a stdlib function call.
///
/// `module` is the canonical module name (e.g. `"io"`, `"math"`, `"string"`).
/// `name` is the function name within that module.
/// `args` is the argument list.
///
/// Returns `None` if no stdlib module matches.
/// Returns `Some(StdlibResult::Value(v))` for synchronous calls.
/// Returns `Some(StdlibResult::NeedsCallbackDispatch(op))` for parallel callbacks.
pub fn dispatch_stdlib(
    module: &str,
    name: &str,
    args: Vec<fidan_runtime::FidanValue>,
) -> Option<Result<StdlibResult, StdlibError>> {
    match module {
        "async" => async_std::dispatch(name, args).map(|result| {
            Ok(match result {
                async_std::AsyncDispatch::Value(v) => StdlibResult::Value(v),
                async_std::AsyncDispatch::Op(op) => StdlibResult::NeedsAsyncDispatch(op),
            })
        }),
        "test" => {
            test_runner::dispatch(name, args).map(|res| {
                Ok(match res {
                    Ok(v) => StdlibResult::Value(v),
                    // Assertion failures are converted to Nothing here; the MIR
                    // interpreter should check for assertion failure panics via
                    // the dedicated dispatch path.
                    Err(msg) => StdlibResult::Value(fidan_runtime::FidanValue::String(
                        fidan_runtime::FidanString::new(&format!("__test_fail__: {msg}")),
                    )),
                })
            })
        }
        "parallel" => parallel::dispatch_op(name, args).map(|res| {
            Ok(match res {
                Ok(Some(op)) => StdlibResult::NeedsCallbackDispatch(op),
                Ok(None) => StdlibResult::Value(fidan_runtime::FidanValue::Nothing),
                Err(msg) => StdlibResult::Value(fidan_runtime::FidanValue::String(
                    fidan_runtime::FidanString::new(&format!("__error__: {msg}")),
                )),
            })
        }),
        "io" => fidan_runtime::stdlib::dispatch_io_module(name, args).map(|res| {
            res.map(StdlibResult::Value).map_err(|err| StdlibError {
                code: err.code,
                message: err.message,
            })
        }),
        "math" => fidan_runtime::stdlib::math::dispatch_result(name, args).map(|res| {
            res.map(StdlibResult::Value).map_err(|err| StdlibError {
                code: err.code,
                message: err.message,
            })
        }),
        "json" => fidan_runtime::stdlib::dispatch_json_module(name, args).map(|res| {
            res.map(StdlibResult::Value).map_err(|err| StdlibError {
                code: err.code,
                message: err.message,
            })
        }),
        _ => fidan_runtime::stdlib::dispatch_value_module(module, name, args)
            .map(|value| Ok(StdlibResult::Value(value))),
    }
}

/// Returns true when `module` is a known stdlib module name.
pub fn is_stdlib_module(module: &str) -> bool {
    module_info(module).is_some()
}

/// Returns all exported function names for a given stdlib module.
/// Used by `use std.module.{name}` to validate name lists at import resolution time.
pub fn module_exports(module: &str) -> &'static [&'static str] {
    module_info(module)
        .map(|info| (info.exports)())
        .unwrap_or(&[])
}

/// Dispatch a test assertion — returns `Err(failure_message)` on failure.
pub fn dispatch_test_assertion(
    name: &str,
    args: Vec<fidan_runtime::FidanValue>,
) -> Option<Result<fidan_runtime::FidanValue, String>> {
    test_runner::dispatch(name, args)
}

pub use metadata::{
    MathIntrinsic, StdlibIntrinsic, StdlibMethodInfo, StdlibTypeSpec, StdlibValueKind,
    infer_precise_stdlib_return_type, infer_receiver_method, infer_stdlib_method,
    parse_stdlib_type_spec,
};

pub use fidan_config::{
    ReceiverBuiltinKind, ReceiverMemberInfo, ReceiverMethodOp, ReceiverParamInfo,
    ReceiverReturnKind, infer_receiver_member, receiver_member_params, receiver_member_signature,
};

#[cfg(test)]
mod tests {
    use super::{
        STDLIB_MODULES, member_doc, member_return_type, module_exports, module_info, module_members,
    };

    #[test]
    fn relocated_metadata_preserves_public_params_and_rendered_alias_signatures() {
        assert!(std::ptr::eq(
            super::member_info("time", "format_date").unwrap(),
            fidan_config::stdlib::member_info("time", "format").unwrap(),
        ));
        assert_eq!(
            super::member_signature("math", "pow").as_deref(),
            Some("std.math.pow(x oftype float, y oftype float) -> float")
        );
        let params = super::member_params("time", "format_date").unwrap();
        assert_eq!(params.len(), 2);
        assert!(params.iter().all(|param| param.optional));
        assert_eq!(
            super::member_signature("string", "parse_int"),
            super::member_signature("string", "parseInt")
        );
    }

    #[test]
    fn module_infos_are_unique() {
        for (i, info) in STDLIB_MODULES.iter().enumerate() {
            assert!(
                STDLIB_MODULES[i + 1..]
                    .iter()
                    .all(|other| other.name != info.name),
                "duplicate stdlib module `{}`",
                info.name
            );
            assert!(module_info(info.name).is_some());
        }
    }

    #[test]
    fn module_exports_are_unique() {
        for info in STDLIB_MODULES {
            let exports = module_exports(info.name);
            for (i, name) in exports.iter().enumerate() {
                assert!(
                    !exports[i + 1..].contains(name),
                    "duplicate export `{}` in std.{}",
                    name,
                    info.name
                );
            }
        }
    }

    #[test]
    fn member_docs_cover_recent_exports() {
        let sleep = member_doc("time", "sleep").expect("missing std.time.sleep doc");
        assert!(sleep.contains("std.time.sleep(ms oftype integer) -> nothing"));

        let gather = member_doc("async", "gather").expect("missing std.async.gather doc");
        assert!(gather.contains(
            "std.async.gather(handles oftype list oftype Pending oftype dynamic) -> Pending oftype list oftype dynamic"
        ));

        let json = member_doc("json", "parse").expect("missing std.json.parse doc");
        assert!(
            json.contains("std.json.parse(text oftype string, soft oftype boolean?) -> dynamic")
        );

        assert!(member_doc("time", "definitely_missing").is_none());
    }

    #[test]
    fn metadata_covers_all_exported_names() {
        for info in STDLIB_MODULES {
            for export in module_exports(info.name) {
                assert!(
                    module_members(info.name)
                        .iter()
                        .any(|member| member.names.contains(export)),
                    "missing stdlib member metadata for std.{}.{}",
                    info.name,
                    export
                );
            }
        }
    }

    #[test]
    fn return_type_metadata_covers_all_exported_names() {
        for info in STDLIB_MODULES {
            for export in module_exports(info.name) {
                assert!(
                    member_return_type(info.name, export).is_some(),
                    "missing stdlib return type metadata for std.{}.{}",
                    info.name,
                    export
                );
            }
        }
    }
}
