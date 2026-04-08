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

/// A dispatched stdlib call result.
pub use sandbox::{SandboxPolicy, SandboxViolation};

#[derive(Clone, Copy)]
pub struct StdlibModuleInfo {
    pub name: &'static str,
    pub exports: fn() -> &'static [&'static str],
    pub doc: &'static str,
}

#[derive(Clone, Copy)]
pub struct StdlibMemberInfo {
    pub names: &'static [&'static str],
    pub signature: &'static str,
    pub doc: &'static str,
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

pub fn module_members(module: &str) -> &'static [StdlibMemberInfo] {
    match module {
        "async" => ASYNC_MEMBER_INFOS,
        "collections" => COLLECTIONS_MEMBER_INFOS,
        "env" => ENV_MEMBER_INFOS,
        "io" => IO_MEMBER_INFOS,
        "json" => JSON_MEMBER_INFOS,
        "math" => MATH_MEMBER_INFOS,
        "parallel" => PARALLEL_MEMBER_INFOS,
        "regex" => REGEX_MEMBER_INFOS,
        "string" => STRING_MEMBER_INFOS,
        "test" => TEST_MEMBER_INFOS,
        "time" => TIME_MEMBER_INFOS,
        _ => &[],
    }
}

pub fn member_info(module: &str, name: &str) -> Option<&'static StdlibMemberInfo> {
    module_members(module)
        .iter()
        .find(|info| info.names.contains(&name))
}

pub fn member_doc(module: &str, name: &str) -> Option<String> {
    let info = member_info(module, name)?;
    let signature = match member_return_type(module, name) {
        Some(ret_type) => format!("{} -> {}", info.signature, ret_type),
        None => info.signature.to_string(),
    };
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
        "std.collections.range(start, end?)" => "list oftype integer",
        "std.collections.Set(items?)" => "dict oftype string oftype boolean",
        "std.collections.setAdd(set, value)" => "nothing",
        "std.collections.setRemove(set, value)" => "nothing",
        "std.collections.setContains(set, value)" => "boolean",
        "std.collections.setToList(set)" => "list oftype string",
        "std.collections.setLen(set)" => "integer",
        "std.collections.setUnion(left, right)" => "dict oftype string oftype boolean",
        "std.collections.setIntersect(left, right)" => "dict oftype string oftype boolean",
        "std.collections.setDiff(left, right)" => "dict oftype string oftype boolean",
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
        "std.collections.groupBy(list)" => "dict oftype string oftype list oftype dynamic",
        "std.collections.unique(list)" => "list oftype dynamic",
        "std.collections.reverse(list)" => "list oftype dynamic",
        "std.collections.sort(list)" => "list oftype dynamic",
        "std.collections.len(list)" => "integer",
        "std.collections.isEmpty(list)" => "boolean",
        "std.collections.concat(left, right)" => "list oftype dynamic",
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
        "std.json.loads(text)" => "dynamic",
        "std.json.parse(text)" => "dynamic",
        "std.json.load(path)" => "dynamic",
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
) -> Option<StdlibResult> {
    match module {
        "async" => async_std::dispatch(name, args).map(|result| match result {
            async_std::AsyncDispatch::Value(v) => StdlibResult::Value(v),
            async_std::AsyncDispatch::Op(op) => StdlibResult::NeedsAsyncDispatch(op),
        }),
        "test" => {
            test_runner::dispatch(name, args).map(|res| {
                match res {
                    Ok(v) => StdlibResult::Value(v),
                    // Assertion failures are converted to Nothing here; the MIR
                    // interpreter should check for assertion failure panics via
                    // the dedicated dispatch path.
                    Err(msg) => StdlibResult::Value(fidan_runtime::FidanValue::String(
                        fidan_runtime::FidanString::new(&format!("__test_fail__: {msg}")),
                    )),
                }
            })
        }
        "parallel" => parallel::dispatch_op(name, args).map(|res| match res {
            Ok(Some(op)) => StdlibResult::NeedsCallbackDispatch(op),
            Ok(None) => StdlibResult::Value(fidan_runtime::FidanValue::Nothing),
            Err(msg) => StdlibResult::Value(fidan_runtime::FidanValue::String(
                fidan_runtime::FidanString::new(&format!("__error__: {msg}")),
            )),
        }),
        _ => fidan_runtime::stdlib::dispatch_value_module(module, name, args)
            .map(StdlibResult::Value),
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
    ReceiverBuiltinKind, ReceiverMemberInfo, ReceiverReturnKind, infer_receiver_member,
};

const ASYNC_MEMBER_INFOS: &[StdlibMemberInfo] = &[
    StdlibMemberInfo {
        names: &["sleep", "wait"],
        signature: "std.async.sleep(ms)",
        doc: "Return a pending handle that resolves after the given number of milliseconds on the same-thread async scheduler.",
    },
    StdlibMemberInfo {
        names: &["ready"],
        signature: "std.async.ready(value)",
        doc: "Wrap an already-available value in a pending handle.",
    },
    StdlibMemberInfo {
        names: &["gather", "waitAll", "wait_all"],
        signature: "std.async.gather(handles)",
        doc: "Wait for every pending handle in the list and collect all results in order.",
    },
    StdlibMemberInfo {
        names: &["waitAny", "wait_any"],
        signature: "std.async.waitAny(handles)",
        doc: "Resolve with the first pending handle that completes, returning its index and value.",
    },
    StdlibMemberInfo {
        names: &["timeout"],
        signature: "std.async.timeout(handle, ms)",
        doc: "Await a pending handle with a timeout and return whether it completed before the deadline.",
    },
];

const COLLECTIONS_MEMBER_INFOS: &[StdlibMemberInfo] = &[
    StdlibMemberInfo {
        names: &["range"],
        signature: "std.collections.range(start, end?)",
        doc: "Create a numeric range as a list of integers.",
    },
    StdlibMemberInfo {
        names: &["Set"],
        signature: "std.collections.Set(items?)",
        doc: "Create a set-like collection of unique values.",
    },
    StdlibMemberInfo {
        names: &["setAdd", "set_add"],
        signature: "std.collections.setAdd(set, value)",
        doc: "Insert a value into a set-like collection.",
    },
    StdlibMemberInfo {
        names: &["setRemove", "set_remove"],
        signature: "std.collections.setRemove(set, value)",
        doc: "Remove a value from a set-like collection.",
    },
    StdlibMemberInfo {
        names: &["setContains", "set_contains"],
        signature: "std.collections.setContains(set, value)",
        doc: "Check whether a set-like collection contains a value.",
    },
    StdlibMemberInfo {
        names: &["setToList", "set_to_list"],
        signature: "std.collections.setToList(set)",
        doc: "Convert a set-like collection to a list.",
    },
    StdlibMemberInfo {
        names: &["setLen", "set_len"],
        signature: "std.collections.setLen(set)",
        doc: "Return the number of values in a set-like collection.",
    },
    StdlibMemberInfo {
        names: &["setUnion", "set_union"],
        signature: "std.collections.setUnion(left, right)",
        doc: "Return the union of two set-like collections.",
    },
    StdlibMemberInfo {
        names: &["setIntersect", "set_intersect"],
        signature: "std.collections.setIntersect(left, right)",
        doc: "Return the intersection of two set-like collections.",
    },
    StdlibMemberInfo {
        names: &["setDiff", "set_diff"],
        signature: "std.collections.setDiff(left, right)",
        doc: "Return the values present in the left set-like collection but not the right one.",
    },
    StdlibMemberInfo {
        names: &["Queue"],
        signature: "std.collections.Queue(items?)",
        doc: "Create a FIFO queue-backed collection.",
    },
    StdlibMemberInfo {
        names: &["enqueue"],
        signature: "std.collections.enqueue(queue, value)",
        doc: "Push a value onto the back of a queue.",
    },
    StdlibMemberInfo {
        names: &["dequeue"],
        signature: "std.collections.dequeue(queue)",
        doc: "Pop and return the front value from a queue.",
    },
    StdlibMemberInfo {
        names: &["peek"],
        signature: "std.collections.peek(queue)",
        doc: "Return the next queue value without removing it.",
    },
    StdlibMemberInfo {
        names: &["Stack"],
        signature: "std.collections.Stack(items?)",
        doc: "Create a LIFO stack-backed collection.",
    },
    StdlibMemberInfo {
        names: &["push"],
        signature: "std.collections.push(stack, value)",
        doc: "Push a value onto the top of a stack.",
    },
    StdlibMemberInfo {
        names: &["pop"],
        signature: "std.collections.pop(stack)",
        doc: "Pop and return the top value from a stack or list-like collection.",
    },
    StdlibMemberInfo {
        names: &["top", "stackPeek", "stack_peek"],
        signature: "std.collections.top(stack)",
        doc: "Return the top stack value without removing it.",
    },
    StdlibMemberInfo {
        names: &["flatten"],
        signature: "std.collections.flatten(list)",
        doc: "Flatten one level of nested list values.",
    },
    StdlibMemberInfo {
        names: &["zip"],
        signature: "std.collections.zip(left, right)",
        doc: "Pair elements from two lists positionally.",
    },
    StdlibMemberInfo {
        names: &["enumerate"],
        signature: "std.collections.enumerate(list)",
        doc: "Pair each list item with its index.",
    },
    StdlibMemberInfo {
        names: &["chunk"],
        signature: "std.collections.chunk(list, size)",
        doc: "Split a list into fixed-size chunks.",
    },
    StdlibMemberInfo {
        names: &["window"],
        signature: "std.collections.window(list, size)",
        doc: "Return sliding windows over a list.",
    },
    StdlibMemberInfo {
        names: &["partition"],
        signature: "std.collections.partition(list)",
        doc: "Split a list into truthy and falsy groups.",
    },
    StdlibMemberInfo {
        names: &["groupBy", "group_by"],
        signature: "std.collections.groupBy(list)",
        doc: "Group list items by key into a dictionary of lists.",
    },
    StdlibMemberInfo {
        names: &["unique", "dedup"],
        signature: "std.collections.unique(list)",
        doc: "Return a list with duplicate values removed.",
    },
    StdlibMemberInfo {
        names: &["reverse"],
        signature: "std.collections.reverse(list)",
        doc: "Return a reversed copy of a list.",
    },
    StdlibMemberInfo {
        names: &["sort"],
        signature: "std.collections.sort(list)",
        doc: "Return a sorted copy of a list.",
    },
    StdlibMemberInfo {
        names: &["count", "length", "len"],
        signature: "std.collections.len(list)",
        doc: "Return the number of items in a collection.",
    },
    StdlibMemberInfo {
        names: &["isEmpty", "is_empty"],
        signature: "std.collections.isEmpty(list)",
        doc: "Return whether a collection has no items.",
    },
    StdlibMemberInfo {
        names: &["concat"],
        signature: "std.collections.concat(left, right)",
        doc: "Concatenate two lists into a new list.",
    },
    StdlibMemberInfo {
        names: &["slice"],
        signature: "std.collections.slice(list, start, end?)",
        doc: "Return a slice of a list.",
    },
    StdlibMemberInfo {
        names: &["first"],
        signature: "std.collections.first(list)",
        doc: "Return the first item in a list or `nothing` when empty.",
    },
    StdlibMemberInfo {
        names: &["last"],
        signature: "std.collections.last(list)",
        doc: "Return the last item in a list or `nothing` when empty.",
    },
    StdlibMemberInfo {
        names: &["join"],
        signature: "std.collections.join(list, separator)",
        doc: "Join a list of displayable values into a string.",
    },
    StdlibMemberInfo {
        names: &["sum"],
        signature: "std.collections.sum(list)",
        doc: "Sum numeric values in a list.",
    },
    StdlibMemberInfo {
        names: &["product"],
        signature: "std.collections.product(list)",
        doc: "Multiply numeric values in a list.",
    },
    StdlibMemberInfo {
        names: &["min"],
        signature: "std.collections.min(list)",
        doc: "Return the minimum numeric value in a list.",
    },
    StdlibMemberInfo {
        names: &["max"],
        signature: "std.collections.max(list)",
        doc: "Return the maximum numeric value in a list.",
    },
];

const ENV_MEMBER_INFOS: &[StdlibMemberInfo] = &[
    StdlibMemberInfo {
        names: &["get", "getVar", "get_var"],
        signature: "std.env.get(key)",
        doc: "Read an environment variable and return `nothing` when it is unset.",
    },
    StdlibMemberInfo {
        names: &["set", "setVar", "set_var"],
        signature: "std.env.set(key, value)",
        doc: "Set an environment variable in the current process.",
    },
    StdlibMemberInfo {
        names: &["args"],
        signature: "std.env.args()",
        doc: "Return the current program arguments passed to the Fidan script.",
    },
];

const IO_MEMBER_INFOS: &[StdlibMemberInfo] = &[
    StdlibMemberInfo {
        names: &["print"],
        signature: "std.io.print(value...)",
        doc: "Print values to stdout followed by a newline.",
    },
    StdlibMemberInfo {
        names: &["eprint"],
        signature: "std.io.eprint(value...)",
        doc: "Print values to stderr followed by a newline.",
    },
    StdlibMemberInfo {
        names: &["readLine", "read_line", "readline"],
        signature: "std.io.readLine(prompt?)",
        doc: "Read one line from stdin, optionally after showing a prompt.",
    },
    StdlibMemberInfo {
        names: &["readFile", "read_file"],
        signature: "std.io.readFile(path)",
        doc: "Read an entire text file into a string.",
    },
    StdlibMemberInfo {
        names: &["readLines", "read_lines"],
        signature: "std.io.readLines(path)",
        doc: "Read a text file into a list of lines.",
    },
    StdlibMemberInfo {
        names: &["writeFile", "write_file"],
        signature: "std.io.writeFile(path, content)",
        doc: "Write a string to a file, replacing existing contents.",
    },
    StdlibMemberInfo {
        names: &["appendFile", "append_file"],
        signature: "std.io.appendFile(path, content)",
        doc: "Append a string to the end of a file, creating it when needed.",
    },
    StdlibMemberInfo {
        names: &["deleteFile", "delete_file"],
        signature: "std.io.deleteFile(path)",
        doc: "Delete a file from disk.",
    },
    StdlibMemberInfo {
        names: &["fileExists", "file_exists", "exists"],
        signature: "std.io.fileExists(path)",
        doc: "Return whether a filesystem path exists.",
    },
    StdlibMemberInfo {
        names: &["isFile", "is_file"],
        signature: "std.io.isFile(path)",
        doc: "Return whether a path points to a file.",
    },
    StdlibMemberInfo {
        names: &["isDir", "is_dir", "isDirectory", "is_directory"],
        signature: "std.io.isDir(path)",
        doc: "Return whether a path points to a directory.",
    },
    StdlibMemberInfo {
        names: &["makeDir", "make_dir", "mkdir", "createDir", "create_dir"],
        signature: "std.io.makeDir(path)",
        doc: "Create a directory and any missing parent directories.",
    },
    StdlibMemberInfo {
        names: &["listDir", "list_dir", "readDir", "read_dir"],
        signature: "std.io.listDir(path)",
        doc: "List directory entry names as strings.",
    },
    StdlibMemberInfo {
        names: &["copyFile", "copy_file"],
        signature: "std.io.copyFile(from, to)",
        doc: "Copy a file from one path to another.",
    },
    StdlibMemberInfo {
        names: &["renameFile", "rename_file", "moveFile", "move_file"],
        signature: "std.io.renameFile(from, to)",
        doc: "Rename or move a file on disk.",
    },
    StdlibMemberInfo {
        names: &["join", "joinPath", "join_path"],
        signature: "std.io.joinPath(part...)",
        doc: "Join path segments into a single path string.",
    },
    StdlibMemberInfo {
        names: &["dirname", "dir_name"],
        signature: "std.io.dirname(path)",
        doc: "Return the parent directory of a path.",
    },
    StdlibMemberInfo {
        names: &["basename", "base_name", "fileName", "file_name"],
        signature: "std.io.basename(path)",
        doc: "Return the final path component of a path.",
    },
    StdlibMemberInfo {
        names: &["extension"],
        signature: "std.io.extension(path)",
        doc: "Return the extension portion of a path.",
    },
    StdlibMemberInfo {
        names: &["cwd", "currentDir", "current_dir"],
        signature: "std.io.cwd()",
        doc: "Return the current working directory.",
    },
    StdlibMemberInfo {
        names: &["absolutePath", "absolute_path"],
        signature: "std.io.absolutePath(path)",
        doc: "Return the canonical absolute path when possible.",
    },
    StdlibMemberInfo {
        names: &["getEnv", "get_env", "env"],
        signature: "std.io.getEnv(key)",
        doc: "Read an environment variable and return `nothing` when it is unset.",
    },
    StdlibMemberInfo {
        names: &["setEnv", "set_env"],
        signature: "std.io.setEnv(key, value)",
        doc: "Set an environment variable in the current process.",
    },
    StdlibMemberInfo {
        names: &["args", "argv"],
        signature: "std.io.args()",
        doc: "Return the current program arguments passed to the Fidan script.",
    },
    StdlibMemberInfo {
        names: &["flush"],
        signature: "std.io.flush()",
        doc: "Flush stdout.",
    },
    StdlibMemberInfo {
        names: &["isatty"],
        signature: "std.io.isatty(stream?)",
        doc: "Return whether stdin, stdout, or stderr is attached to a terminal.",
    },
];

const JSON_MEMBER_INFOS: &[StdlibMemberInfo] = &[
    StdlibMemberInfo {
        names: &["loads"],
        signature: "std.json.loads(text)",
        doc: "Parse JSON text into Fidan dynamic values using dict, list, string, number, boolean, and nothing.",
    },
    StdlibMemberInfo {
        names: &["parse"],
        signature: "std.json.parse(text)",
        doc: "Parse JSON text into Fidan dynamic values using dict, list, string, number, boolean, and nothing. `parse` is a compatibility alias for `loads`.",
    },
    StdlibMemberInfo {
        names: &["load", "readFile", "read_file"],
        signature: "std.json.load(path)",
        doc: "Read a JSON file from disk and parse it into Fidan dynamic values.",
    },
    StdlibMemberInfo {
        names: &["dumps"],
        signature: "std.json.dumps(value)",
        doc: "Serialize a JSON-compatible Fidan value into compact JSON text. Unsupported runtime-only values are stringified via their display form.",
    },
    StdlibMemberInfo {
        names: &["stringify"],
        signature: "std.json.stringify(value)",
        doc: "Serialize a JSON-compatible Fidan value into compact JSON text. `stringify` is a compatibility alias for `dumps`. Unsupported runtime-only values are stringified via their display form.",
    },
    StdlibMemberInfo {
        names: &["dump", "writeFile", "write_file"],
        signature: "std.json.dump(value, path)",
        doc: "Serialize a JSON-compatible Fidan value and write it directly to a file path, returning whether the write succeeded.",
    },
    StdlibMemberInfo {
        names: &["pretty", "prettyPrint", "pretty_print"],
        signature: "std.json.pretty(value)",
        doc: "Serialize a JSON-compatible Fidan value into indented JSON text for logs, snapshots, and debugging output.",
    },
    StdlibMemberInfo {
        names: &["isValid", "is_valid"],
        signature: "std.json.isValid(text)",
        doc: "Return whether the provided string is valid JSON without materializing the parsed value.",
    },
];

const MATH_MEMBER_INFOS: &[StdlibMemberInfo] = &[
    StdlibMemberInfo {
        names: &["sin"],
        signature: "std.math.sin(x)",
        doc: "Return the sine of `x` in radians.",
    },
    StdlibMemberInfo {
        names: &["cos"],
        signature: "std.math.cos(x)",
        doc: "Return the cosine of `x` in radians.",
    },
    StdlibMemberInfo {
        names: &["tan"],
        signature: "std.math.tan(x)",
        doc: "Return the tangent of `x` in radians.",
    },
    StdlibMemberInfo {
        names: &["asin"],
        signature: "std.math.asin(x)",
        doc: "Return the inverse sine of `x`.",
    },
    StdlibMemberInfo {
        names: &["acos"],
        signature: "std.math.acos(x)",
        doc: "Return the inverse cosine of `x`.",
    },
    StdlibMemberInfo {
        names: &["atan"],
        signature: "std.math.atan(x)",
        doc: "Return the inverse tangent of `x`.",
    },
    StdlibMemberInfo {
        names: &["atan2"],
        signature: "std.math.atan2(y, x)",
        doc: "Return the quadrant-aware arctangent of `y / x`.",
    },
    StdlibMemberInfo {
        names: &["sinh"],
        signature: "std.math.sinh(x)",
        doc: "Return the hyperbolic sine of `x`.",
    },
    StdlibMemberInfo {
        names: &["cosh"],
        signature: "std.math.cosh(x)",
        doc: "Return the hyperbolic cosine of `x`.",
    },
    StdlibMemberInfo {
        names: &["tanh"],
        signature: "std.math.tanh(x)",
        doc: "Return the hyperbolic tangent of `x`.",
    },
    StdlibMemberInfo {
        names: &["sqrt"],
        signature: "std.math.sqrt(x)",
        doc: "Return the square root of `x`.",
    },
    StdlibMemberInfo {
        names: &["cbrt"],
        signature: "std.math.cbrt(x)",
        doc: "Return the cube root of `x`.",
    },
    StdlibMemberInfo {
        names: &["pow"],
        signature: "std.math.pow(x, y)",
        doc: "Raise `x` to the power `y`.",
    },
    StdlibMemberInfo {
        names: &["exp"],
        signature: "std.math.exp(x)",
        doc: "Return `e^x`.",
    },
    StdlibMemberInfo {
        names: &["exp2"],
        signature: "std.math.exp2(x)",
        doc: "Return `2^x`.",
    },
    StdlibMemberInfo {
        names: &["log"],
        signature: "std.math.log(x)",
        doc: "Return the natural logarithm of `x`.",
    },
    StdlibMemberInfo {
        names: &["log2"],
        signature: "std.math.log2(x)",
        doc: "Return the base-2 logarithm of `x`.",
    },
    StdlibMemberInfo {
        names: &["log10"],
        signature: "std.math.log10(x)",
        doc: "Return the base-10 logarithm of `x`.",
    },
    StdlibMemberInfo {
        names: &["logN", "log_n"],
        signature: "std.math.logN(x, base)",
        doc: "Return the logarithm of `x` in an arbitrary base.",
    },
    StdlibMemberInfo {
        names: &["floor"],
        signature: "std.math.floor(x)",
        doc: "Round `x` down to the nearest integer.",
    },
    StdlibMemberInfo {
        names: &["ceil"],
        signature: "std.math.ceil(x)",
        doc: "Round `x` up to the nearest integer.",
    },
    StdlibMemberInfo {
        names: &["round"],
        signature: "std.math.round(x)",
        doc: "Round `x` to the nearest integer.",
    },
    StdlibMemberInfo {
        names: &["trunc"],
        signature: "std.math.trunc(x)",
        doc: "Truncate the fractional part of `x`.",
    },
    StdlibMemberInfo {
        names: &["fract"],
        signature: "std.math.fract(x)",
        doc: "Return the fractional part of `x`.",
    },
    StdlibMemberInfo {
        names: &["abs"],
        signature: "std.math.abs(x)",
        doc: "Return the absolute value of `x`.",
    },
    StdlibMemberInfo {
        names: &["sign", "signum"],
        signature: "std.math.sign(x)",
        doc: "Return the sign of `x` as `-1`, `0`, or `1`.",
    },
    StdlibMemberInfo {
        names: &["min"],
        signature: "std.math.min(a, b)",
        doc: "Return the smaller of two numeric values.",
    },
    StdlibMemberInfo {
        names: &["max"],
        signature: "std.math.max(a, b)",
        doc: "Return the larger of two numeric values.",
    },
    StdlibMemberInfo {
        names: &["clamp"],
        signature: "std.math.clamp(x, lo, hi)",
        doc: "Clamp `x` into the inclusive range `[lo, hi]`.",
    },
    StdlibMemberInfo {
        names: &["hypot"],
        signature: "std.math.hypot(x, y)",
        doc: "Return the Euclidean length of a 2D vector.",
    },
    StdlibMemberInfo {
        names: &["pi", "PI"],
        signature: "std.math.pi()",
        doc: "Return the constant π.",
    },
    StdlibMemberInfo {
        names: &["e", "E"],
        signature: "std.math.e()",
        doc: "Return Euler's number.",
    },
    StdlibMemberInfo {
        names: &["tau", "TAU"],
        signature: "std.math.tau()",
        doc: "Return the constant τ.",
    },
    StdlibMemberInfo {
        names: &["inf", "infinity"],
        signature: "std.math.inf()",
        doc: "Return positive infinity.",
    },
    StdlibMemberInfo {
        names: &["nan", "NaN"],
        signature: "std.math.nan()",
        doc: "Return a NaN floating-point value.",
    },
    StdlibMemberInfo {
        names: &["isNan", "isNaN", "is_nan"],
        signature: "std.math.isNan(x)",
        doc: "Return whether `x` is NaN.",
    },
    StdlibMemberInfo {
        names: &["isInfinite", "is_infinite"],
        signature: "std.math.isInfinite(x)",
        doc: "Return whether `x` is infinite.",
    },
    StdlibMemberInfo {
        names: &["isFinite", "is_finite"],
        signature: "std.math.isFinite(x)",
        doc: "Return whether `x` is finite.",
    },
    StdlibMemberInfo {
        names: &["random"],
        signature: "std.math.random()",
        doc: "Return a pseudo-random float in the range `[0, 1]`.",
    },
    StdlibMemberInfo {
        names: &["randomInt", "random_int"],
        signature: "std.math.randomInt(lo, hi)",
        doc: "Return a pseudo-random integer in the half-open range `[lo, hi)`.",
    },
    StdlibMemberInfo {
        names: &["toDeg", "to_deg", "degrees"],
        signature: "std.math.toDeg(x)",
        doc: "Convert radians to degrees.",
    },
    StdlibMemberInfo {
        names: &["toRad", "to_rad", "radians"],
        signature: "std.math.toRad(x)",
        doc: "Convert degrees to radians.",
    },
];

const PARALLEL_MEMBER_INFOS: &[StdlibMemberInfo] = &[
    StdlibMemberInfo {
        names: &["parallelMap", "parallel_map"],
        signature: "std.parallel.parallelMap(list, fn)",
        doc: "Apply a function to every list item on worker threads and collect the results.",
    },
    StdlibMemberInfo {
        names: &["parallelFilter", "parallel_filter"],
        signature: "std.parallel.parallelFilter(list, fn)",
        doc: "Filter a list on worker threads using a predicate function.",
    },
    StdlibMemberInfo {
        names: &["parallelForEach", "parallel_for_each"],
        signature: "std.parallel.parallelForEach(list, fn)",
        doc: "Run a function over every list item on worker threads for side effects.",
    },
    StdlibMemberInfo {
        names: &["parallelReduce", "parallel_reduce"],
        signature: "std.parallel.parallelReduce(list, init, fn)",
        doc: "Reduce a list on worker threads using an associative reducer function.",
    },
];

const REGEX_MEMBER_INFOS: &[StdlibMemberInfo] = &[
    StdlibMemberInfo {
        names: &["test", "isMatch", "is_match"],
        signature: "std.regex.test(pattern, subject)",
        doc: "Return whether the pattern matches the subject string.",
    },
    StdlibMemberInfo {
        names: &["match", "find", "find_first"],
        signature: "std.regex.match(pattern, subject)",
        doc: "Return the first match or `nothing` when no match exists.",
    },
    StdlibMemberInfo {
        names: &["findAll", "find_all", "matches"],
        signature: "std.regex.findAll(pattern, subject)",
        doc: "Return every match as a list of strings.",
    },
    StdlibMemberInfo {
        names: &["capture", "exec"],
        signature: "std.regex.capture(pattern, subject)",
        doc: "Return the first match and capture groups as a list.",
    },
    StdlibMemberInfo {
        names: &["captureAll", "capture_all", "execAll", "exec_all"],
        signature: "std.regex.captureAll(pattern, subject)",
        doc: "Return all match capture groups as a list of lists.",
    },
    StdlibMemberInfo {
        names: &["replace", "replaceFirst", "replace_first", "sub"],
        signature: "std.regex.replace(pattern, subject, replacement)",
        doc: "Replace the first regex match in a string.",
    },
    StdlibMemberInfo {
        names: &["replaceAll", "replace_all", "gsub"],
        signature: "std.regex.replaceAll(pattern, subject, replacement)",
        doc: "Replace every regex match in a string.",
    },
    StdlibMemberInfo {
        names: &["split"],
        signature: "std.regex.split(pattern, subject)",
        doc: "Split a string by a regex pattern.",
    },
    StdlibMemberInfo {
        names: &["isValid", "is_valid"],
        signature: "std.regex.isValid(pattern)",
        doc: "Return whether a regex pattern compiles successfully.",
    },
];

const STRING_MEMBER_INFOS: &[StdlibMemberInfo] = &[
    StdlibMemberInfo {
        names: &["toUpper", "upper", "to_upper"],
        signature: "std.string.toUpper(text)",
        doc: "Uppercase a string.",
    },
    StdlibMemberInfo {
        names: &["toLower", "lower", "to_lower"],
        signature: "std.string.toLower(text)",
        doc: "Lowercase a string.",
    },
    StdlibMemberInfo {
        names: &["capitalize"],
        signature: "std.string.capitalize(text)",
        doc: "Uppercase the first character of a string.",
    },
    StdlibMemberInfo {
        names: &["trim"],
        signature: "std.string.trim(text)",
        doc: "Trim leading and trailing whitespace.",
    },
    StdlibMemberInfo {
        names: &["trimStart", "ltrim", "trim_start"],
        signature: "std.string.trimStart(text)",
        doc: "Trim leading whitespace.",
    },
    StdlibMemberInfo {
        names: &["trimEnd", "rtrim", "trim_end"],
        signature: "std.string.trimEnd(text)",
        doc: "Trim trailing whitespace.",
    },
    StdlibMemberInfo {
        names: &["split"],
        signature: "std.string.split(text, separator)",
        doc: "Split a string into a list.",
    },
    StdlibMemberInfo {
        names: &["join"],
        signature: "std.string.join(separator, list)",
        doc: "Join a list of values into a string.",
    },
    StdlibMemberInfo {
        names: &["lines"],
        signature: "std.string.lines(text)",
        doc: "Split a string into lines.",
    },
    StdlibMemberInfo {
        names: &["contains"],
        signature: "std.string.contains(text, pattern)",
        doc: "Return whether a string contains a substring.",
    },
    StdlibMemberInfo {
        names: &["startsWith", "starts_with"],
        signature: "std.string.startsWith(text, prefix)",
        doc: "Return whether a string starts with a prefix.",
    },
    StdlibMemberInfo {
        names: &["endsWith", "ends_with"],
        signature: "std.string.endsWith(text, suffix)",
        doc: "Return whether a string ends with a suffix.",
    },
    StdlibMemberInfo {
        names: &["indexOf", "index_of"],
        signature: "std.string.indexOf(text, pattern)",
        doc: "Return the first substring index or `-1`.",
    },
    StdlibMemberInfo {
        names: &["lastIndexOf", "last_index_of"],
        signature: "std.string.lastIndexOf(text, pattern)",
        doc: "Return the last substring index or `-1`.",
    },
    StdlibMemberInfo {
        names: &["replace"],
        signature: "std.string.replace(text, from, to)",
        doc: "Replace every exact substring occurrence.",
    },
    StdlibMemberInfo {
        names: &["replaceFirst", "replace_first"],
        signature: "std.string.replaceFirst(text, from, to)",
        doc: "Replace the first exact substring occurrence.",
    },
    StdlibMemberInfo {
        names: &["slice", "substr"],
        signature: "std.string.slice(text, start, end?)",
        doc: "Return a substring by character range.",
    },
    StdlibMemberInfo {
        names: &["padStart", "pad_start"],
        signature: "std.string.padStart(text, width, pad?)",
        doc: "Pad a string on the left to a minimum width.",
    },
    StdlibMemberInfo {
        names: &["padEnd", "pad_end"],
        signature: "std.string.padEnd(text, width, pad?)",
        doc: "Pad a string on the right to a minimum width.",
    },
    StdlibMemberInfo {
        names: &["repeat"],
        signature: "std.string.repeat(text, n)",
        doc: "Repeat a string `n` times.",
    },
    StdlibMemberInfo {
        names: &["reverse"],
        signature: "std.string.reverse(text)",
        doc: "Reverse a string by characters.",
    },
    StdlibMemberInfo {
        names: &["len", "length"],
        signature: "std.string.len(text)",
        doc: "Return the character length of a string.",
    },
    StdlibMemberInfo {
        names: &["isEmpty", "is_empty"],
        signature: "std.string.isEmpty(text)",
        doc: "Return whether a string is empty.",
    },
    StdlibMemberInfo {
        names: &["format"],
        signature: "std.string.format(template, value...)",
        doc: "Replace `{}` placeholders in order with values.",
    },
    StdlibMemberInfo {
        names: &["parseInt", "parse_int"],
        signature: "std.string.parseInt(text)",
        doc: "Parse a string into an integer or return `nothing`.",
    },
    StdlibMemberInfo {
        names: &["parseFloat", "parse_float"],
        signature: "std.string.parseFloat(text)",
        doc: "Parse a string into a float or return `nothing`.",
    },
    StdlibMemberInfo {
        names: &["chars"],
        signature: "std.string.chars(text)",
        doc: "Return the characters of a string as a list of one-character strings.",
    },
    StdlibMemberInfo {
        names: &["bytes"],
        signature: "std.string.bytes(text)",
        doc: "Return the bytes of a string as integers.",
    },
    StdlibMemberInfo {
        names: &["fromChars", "from_chars"],
        signature: "std.string.fromChars(chars)",
        doc: "Build a string from a list of one-character strings.",
    },
    StdlibMemberInfo {
        names: &["charCode", "char_code"],
        signature: "std.string.charCode(text)",
        doc: "Return the codepoint of the first character in a string.",
    },
    StdlibMemberInfo {
        names: &["fromCharCode", "from_char_code"],
        signature: "std.string.fromCharCode(code)",
        doc: "Build a one-character string from a numeric codepoint.",
    },
];

const TEST_MEMBER_INFOS: &[StdlibMemberInfo] = &[
    StdlibMemberInfo {
        names: &["assert"],
        signature: "std.test.assert(condition, message?)",
        doc: "Fail when a condition is not truthy.",
    },
    StdlibMemberInfo {
        names: &["assertEq", "assert_eq"],
        signature: "std.test.assertEq(left, right, message?)",
        doc: "Fail when two values are not equal.",
    },
    StdlibMemberInfo {
        names: &["assertNe", "assert_ne"],
        signature: "std.test.assertNe(left, right, message?)",
        doc: "Fail when two values are equal.",
    },
    StdlibMemberInfo {
        names: &["assertGt", "assert_gt"],
        signature: "std.test.assertGt(left, right, message?)",
        doc: "Fail when the left value is not greater than the right value.",
    },
    StdlibMemberInfo {
        names: &["assertLt", "assert_lt"],
        signature: "std.test.assertLt(left, right, message?)",
        doc: "Fail when the left value is not less than the right value.",
    },
    StdlibMemberInfo {
        names: &["assertSome", "assert_some"],
        signature: "std.test.assertSome(value, message?)",
        doc: "Fail when a value is `nothing`.",
    },
    StdlibMemberInfo {
        names: &["assertNothing", "assert_nothing"],
        signature: "std.test.assertNothing(value, message?)",
        doc: "Fail when a value is not `nothing`.",
    },
    StdlibMemberInfo {
        names: &["assertType", "assert_type"],
        signature: "std.test.assertType(value, typeName, message?)",
        doc: "Fail when a value does not match the expected type name.",
    },
    StdlibMemberInfo {
        names: &["fail"],
        signature: "std.test.fail(message?)",
        doc: "Unconditionally fail the current test.",
    },
    StdlibMemberInfo {
        names: &["skip"],
        signature: "std.test.skip(message?)",
        doc: "Mark the current test as skipped.",
    },
];

const TIME_MEMBER_INFOS: &[StdlibMemberInfo] = &[
    StdlibMemberInfo {
        names: &["now"],
        signature: "std.time.now()",
        doc: "Return the current wall-clock time in milliseconds since the Unix epoch.",
    },
    StdlibMemberInfo {
        names: &["timestamp"],
        signature: "std.time.timestamp()",
        doc: "Return the current Unix timestamp in seconds.",
    },
    StdlibMemberInfo {
        names: &["sleep", "wait"],
        signature: "std.time.sleep(ms)",
        doc: "Block the current thread for the given number of milliseconds.",
    },
    StdlibMemberInfo {
        names: &["elapsed"],
        signature: "std.time.elapsed(startMs)",
        doc: "Return elapsed milliseconds since a captured start timestamp.",
    },
    StdlibMemberInfo {
        names: &["date", "today"],
        signature: "std.time.date(ms?)",
        doc: "Format a timestamp as `YYYY-MM-DD`.",
    },
    StdlibMemberInfo {
        names: &["time", "timeStr", "time_str"],
        signature: "std.time.time(ms?)",
        doc: "Format a timestamp as `HH:MM:SS`.",
    },
    StdlibMemberInfo {
        names: &["datetime"],
        signature: "std.time.datetime(ms?)",
        doc: "Format a timestamp as `YYYY-MM-DD HH:MM:SS`.",
    },
    StdlibMemberInfo {
        names: &["format", "formatDate", "format_date"],
        signature: "std.time.format(ms?, pattern)",
        doc: "Format a timestamp with custom date/time tokens.",
    },
    StdlibMemberInfo {
        names: &["year"],
        signature: "std.time.year(ms?)",
        doc: "Extract the year from a timestamp.",
    },
    StdlibMemberInfo {
        names: &["month"],
        signature: "std.time.month(ms?)",
        doc: "Extract the month from a timestamp.",
    },
    StdlibMemberInfo {
        names: &["day"],
        signature: "std.time.day(ms?)",
        doc: "Extract the day of month from a timestamp.",
    },
    StdlibMemberInfo {
        names: &["hour"],
        signature: "std.time.hour(ms?)",
        doc: "Extract the hour from a timestamp.",
    },
    StdlibMemberInfo {
        names: &["minute"],
        signature: "std.time.minute(ms?)",
        doc: "Extract the minute from a timestamp.",
    },
    StdlibMemberInfo {
        names: &["second"],
        signature: "std.time.second(ms?)",
        doc: "Extract the second from a timestamp.",
    },
    StdlibMemberInfo {
        names: &["weekday"],
        signature: "std.time.weekday(ms?)",
        doc: "Extract the weekday index from a timestamp.",
    },
];

#[cfg(test)]
mod tests {
    use super::{
        STDLIB_MODULES, member_doc, member_return_type, module_exports, module_info, module_members,
    };

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
        assert!(sleep.contains("std.time.sleep"));

        let gather = member_doc("async", "gather").expect("missing std.async.gather doc");
        assert!(gather.contains("std.async.gather"));

        let json = member_doc("json", "parse").expect("missing std.json.parse doc");
        assert!(json.contains("std.json.parse"));

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
