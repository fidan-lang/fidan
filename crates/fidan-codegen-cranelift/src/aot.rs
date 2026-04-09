// fidan-codegen-cranelift/src/aot.rs
//
// Cranelift AOT backend — emits a native object file from a `MirProgram` and
// then invokes the system linker to produce a finished binary.
//
// ## Strategy
//
// All Fidan values are passed through the C-ABI runtime: scalars use native
// types (I64 / F64) on the fast path; heap-allocated values are opaque `ptr`
// arguments handled by `fidan-runtime` function calls.
//
// ## Implementation
//
// Each `MirFunction` is lowered to a Cranelift IR function.  The entry-point
// glue (`main`) is synthesised to call `fdn_init()` (always) and `fdn_main()`
// (if the program declares a `main` action).
//
// External C-ABI symbols are imported from `fidan-runtime` via the
// `cranelift_module::Module::declare_function` facility.  The object file
// produced by `cranelift_object::ObjectModule` is then linked with:
//   Unix:    `cc <obj> -lfidan_runtime -lpthread -ldl -lm -o <out>`
//   Windows: `link.exe <obj> fidan_runtime.lib /OUT:<out>`

use anyhow::{Context as _, Result, anyhow, bail};
use cranelift_codegen::{
    Context,
    ir::{
        AbiParam, BlockArg, Function, InstBuilder, MemFlags, TrapCode, UserFuncName,
        condcodes::{FloatCC, IntCC},
        types::{F64, I8, I32, I64},
    },
    isa,
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataDescription, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use fidan_lexer::SymbolInterner;
use fidan_mir::{
    BlockId as MirBlockId, Callee, FunctionId as MirFunctionId, GlobalId, Instr, LocalId,
    MirExternAbi, MirFunction, MirLit, MirProgram, MirStringPart, MirTy, Operand, Rvalue,
    Terminator, collect_effective_local_types, collect_may_throw_functions,
};
use fidan_stdlib::{
    MathIntrinsic, ReceiverBuiltinKind, ReceiverMethodOp, StdlibIntrinsic, StdlibValueKind,
    infer_receiver_member, infer_stdlib_method, is_stdlib_module, module_exports,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use target_lexicon::{Architecture, Triple};

/// Optimisation level for the Cranelift AOT backend.
/// (Cranelift does not expose as many levels as LLVM.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptLevel {
    None,
    /// Speed optimisations (Cranelift's `speed` preset).
    #[default]
    Speed,
    /// Maximum speed; may increase compile time.
    SpeedAndSize,
}

/// Link-time optimization mode for the Cranelift AOT linker step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LtoMode {
    #[default]
    Off,
    /// Best-effort linker-driven LTO for toolchains that support it.
    Full,
}

/// Post-link stripping mode for Cranelift AOT binaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StripMode {
    #[default]
    Off,
    Symbols,
    All,
}

/// Options for a Cranelift AOT compilation.
pub struct AotOptions {
    /// Path for the final binary.
    pub output: PathBuf,
    pub opt_level: OptLevel,
    pub lto: LtoMode,
    pub strip: StripMode,
    /// Emit the object file (`.o`) even after linking succeeds.
    pub emit_obj: bool,
    /// Extra `-L` / `/LIBPATH:` dirs for the linker.
    pub extra_lib_dirs: Vec<PathBuf>,
    /// Link the Fidan runtime as a shared library.
    pub link_dynamic: bool,
    /// Optional target CPU hint.
    ///
    /// Cranelift supports:
    /// - omitted / `native`: use the host ISA via `cranelift_native`
    /// - `generic`: use the generic ISA for the current host triple
    /// - custom presets/features: use the host triple with explicit Cranelift
    ///   CPU presets and ISA feature flags
    pub target_cpu: Option<String>,
}

impl Default for AotOptions {
    fn default() -> Self {
        AotOptions {
            output: PathBuf::from("a.out"),
            opt_level: OptLevel::Speed,
            lto: LtoMode::Off,
            strip: StripMode::Off,
            emit_obj: false,
            extra_lib_dirs: vec![],
            link_dynamic: false,
            target_cpu: None,
        }
    }
}

/// The Cranelift AOT compiler.
pub struct AotCompiler;

impl AotCompiler {
    /// Compile a `MirProgram` to a native binary via Cranelift.
    pub fn compile(
        program: &MirProgram,
        interner: Arc<SymbolInterner>,
        opts: &AotOptions,
    ) -> Result<PathBuf> {
        // ── Build ISA / settings ───────────────────────────────────────────────
        let mut flag_builder = settings::builder();
        let opt_str = match opts.opt_level {
            OptLevel::None => "none",
            OptLevel::Speed => "speed",
            OptLevel::SpeedAndSize => "speed_and_size",
        };
        flag_builder
            .set("opt_level", opt_str)
            .expect("Cranelift: unknown opt_level");
        // Enable position-independent code so the object file is relocatable.
        flag_builder
            .set("is_pic", "true")
            .expect("Cranelift: unknown is_pic");
        let flags = settings::Flags::new(flag_builder);
        let isa = build_target_isa(opts.target_cpu.as_deref(), flags)?;

        // ── Build ObjectModule ────────────────────────────────────────────────
        let module_name = opts
            .output
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("fidan_module");
        let obj_builder =
            ObjectBuilder::new(isa, module_name, cranelift_module::default_libcall_names())
                .map_err(|e| anyhow::anyhow!("Cranelift ObjectBuilder: {e}"))?;
        let mut module = ObjectModule::new(obj_builder);

        // ── Declare all external C-ABI runtime symbols ─────────────────────────
        let rt = RuntimeDecls::declare(&mut module)?;
        let function_throw_map = collect_may_throw_functions(program);

        // ── Forward-declare all Fidan functions ────────────────────────────────
        let fn_ids = declare_all_functions(&mut module, program, &interner)?;
        let extern_fn_ids = declare_all_extern_functions(&mut module, program)?;

        // ── Declare writable global data slots (one 8-byte slot per MirGlobal) ──
        let global_data_ids: Vec<cranelift_module::DataId> = program
            .globals
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let _ = g;
                let mut desc = DataDescription::new();
                desc.define_zeroinit(8);
                let data_id = module
                    .declare_data(
                        &format!("__fidan_global_{i}"),
                        Linkage::Local,
                        true,  // writable
                        false, // not tls
                    )
                    .context("declaring global data slot")?;
                module
                    .define_data(data_id, &desc)
                    .context("defining global data slot")?;
                Ok(data_id)
            })
            .collect::<Result<Vec<_>>>()?;

        // ── Lower each function ────────────────────────────────────────────────
        let mut builder_ctx = FunctionBuilderContext::new();
        let mut ctx = module.make_context();

        for mir_fn in &program.functions {
            ctx.func.clear();
            lower_function(
                &mut module,
                &rt,
                &fn_ids,
                &extern_fn_ids,
                &global_data_ids,
                &function_throw_map,
                &mut ctx,
                &mut builder_ctx,
                program,
                mir_fn,
                &interner,
            )
            .with_context(|| format!("lowering function {:?}", mir_fn.id))?;
            let fn_id = fn_ids[mir_fn.id.0 as usize];
            module
                .define_function(fn_id, &mut ctx)
                .with_context(|| format!("defining function {:?}", mir_fn.id))?;
            module.clear_context(&mut ctx);
        }

        // ── Emit C `main()` entry point ────────────────────────────────────────
        let trampoline_ids = emit_trampolines(
            &mut module,
            &rt,
            &fn_ids,
            &mut ctx,
            &mut builder_ctx,
            program,
        )?;

        emit_c_main(
            &mut module,
            &rt,
            &fn_ids,
            &trampoline_ids,
            &mut ctx,
            &mut builder_ctx,
            program,
            &interner,
        )?;

        // ── Finalise and write object file ─────────────────────────────────────
        let obj = module
            .finish()
            .emit()
            .context("Cranelift: failed to emit object file")?;

        let obj_path = opts
            .output
            .with_extension(if cfg!(windows) { "obj" } else { "o" });
        std::fs::write(&obj_path, &obj)
            .with_context(|| format!("writing object file to {:?}", obj_path))?;

        // ── Link ───────────────────────────────────────────────────────────────
        link(
            &obj_path,
            &opts.output,
            &opts.extra_lib_dirs,
            opts.link_dynamic,
            opts.lto,
            &collect_extern_link_inputs(program),
        )
        .context("Cranelift AOT: linker failed")?;

        if opts.strip != StripMode::Off {
            strip_binary(&opts.output, opts.strip).context("Cranelift AOT: strip failed")?;
        }

        if !opts.emit_obj {
            let _ = std::fs::remove_file(&obj_path);
        }

        Ok(opts.output.clone())
    }
}

fn build_target_isa(
    target_cpu: Option<&str>,
    flags: settings::Flags,
) -> Result<cranelift_codegen::isa::OwnedTargetIsa> {
    let triple = current_host_triple();
    match target_cpu.map(str::trim) {
        None | Some("") | Some("native") => build_native_target_isa(flags),
        Some(spec) if spec.eq_ignore_ascii_case("generic") => {
            build_generic_target_isa(triple, flags)
        }
        Some(spec) => build_custom_target_isa(triple, spec, flags),
    }
}

fn build_native_target_isa(
    flags: settings::Flags,
) -> Result<cranelift_codegen::isa::OwnedTargetIsa> {
    cranelift_native::builder()
        .map_err(|e| anyhow!("cranelift-native: unsupported host: {e}"))?
        .finish(flags)
        .map_err(|e| anyhow!("cranelift-native: ISA build failed: {e}"))
}

fn build_generic_target_isa(
    triple: Triple,
    flags: settings::Flags,
) -> Result<cranelift_codegen::isa::OwnedTargetIsa> {
    isa::lookup(triple)
        .map_err(|e| anyhow!("cranelift generic ISA lookup failed: {e}"))?
        .finish(flags)
        .map_err(|e| anyhow!("cranelift generic ISA build failed: {e}"))
}

fn build_custom_target_isa(
    triple: Triple,
    spec: &str,
    flags: settings::Flags,
) -> Result<cranelift_codegen::isa::OwnedTargetIsa> {
    let (cpu, feature_suffix) = split_target_cpu_spec(spec)?;
    let mut builder = isa::lookup(triple.clone())
        .map_err(|e| anyhow!("cranelift custom ISA lookup failed: {e}"))?;

    if !cpu.eq_ignore_ascii_case("generic") {
        let preset = normalize_target_cpu_preset(triple.architecture, cpu);
        builder.enable(preset.as_str()).map_err(|err| {
            anyhow!(
                "Cranelift target CPU preset `{cpu}` is not supported for `{}`: {err}",
                triple
            )
        })?;
    }

    apply_target_cpu_features(&mut builder, triple.architecture, feature_suffix)
        .map_err(|err| anyhow!("Cranelift target CPU spec `{spec}` is invalid: {err}"))?;

    builder
        .finish(flags)
        .map_err(|e| anyhow!("cranelift custom ISA build failed: {e}"))
}

fn split_target_cpu_spec(spec: &str) -> Result<(&str, &str)> {
    let trimmed = spec.trim();
    let (cpu, features) = match trimmed.find(',') {
        Some(index) => (&trimmed[..index], &trimmed[index + 1..]),
        None => (trimmed, ""),
    };
    let cpu = cpu.trim();
    if cpu.is_empty() {
        bail!("target CPU spec `{trimmed}` is missing a CPU preset or `generic`");
    }
    Ok((cpu, features))
}

fn normalize_target_cpu_preset(arch: Architecture, cpu: &str) -> String {
    let trimmed = cpu.trim();
    match arch {
        Architecture::X86_64 => trimmed.replace('_', "-"),
        _ => trimmed.to_string(),
    }
}

fn apply_target_cpu_features(
    builder: &mut impl Configurable,
    arch: Architecture,
    features: &str,
) -> Result<()> {
    for raw in features.split(',') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        if token.len() < 2 || !matches!(token.as_bytes()[0], b'+' | b'-') {
            bail!(
                "target CPU feature `{token}` must start with `+` or `-` (for example `+avx2` or `-has_avx2`)"
            );
        }
        let enabled = token.as_bytes()[0] == b'+';
        let feature = token[1..].trim();
        if feature.is_empty() {
            bail!("target CPU feature override `{token}` is missing a feature name");
        }
        let name = map_target_cpu_feature_name(arch, feature);
        builder
            .set(name.as_str(), if enabled { "true" } else { "false" })
            .map_err(|err| {
                anyhow!(
                    "target CPU feature `{feature}` is not supported for `{}`: {err}",
                    architecture_name(arch)
                )
            })?;
    }
    Ok(())
}

fn map_target_cpu_feature_name(arch: Architecture, feature: &str) -> String {
    let trimmed = feature.trim();
    if trimmed.starts_with("has_")
        || trimmed.starts_with("use_")
        || trimmed.starts_with("sign_return_address")
    {
        return trimmed.to_string();
    }

    match arch {
        Architecture::X86_64 => match trimmed.to_ascii_lowercase().as_str() {
            "cx16" | "cmpxchg16b" => "has_cmpxchg16b".to_string(),
            "sse3" => "has_sse3".to_string(),
            "ssse3" => "has_ssse3".to_string(),
            "sse4.1" | "sse4_1" | "sse41" => "has_sse41".to_string(),
            "sse4.2" | "sse4_2" | "sse42" => "has_sse42".to_string(),
            "popcnt" => "has_popcnt".to_string(),
            "avx" => "has_avx".to_string(),
            "avx2" => "has_avx2".to_string(),
            "fma" => "has_fma".to_string(),
            "bmi1" => "has_bmi1".to_string(),
            "bmi2" => "has_bmi2".to_string(),
            "avx512bitalg" => "has_avx512bitalg".to_string(),
            "avx512dq" => "has_avx512dq".to_string(),
            "avx512f" => "has_avx512f".to_string(),
            "avx512vl" => "has_avx512vl".to_string(),
            "avx512vbmi" => "has_avx512vbmi".to_string(),
            "lzcnt" => "has_lzcnt".to_string(),
            other => other.to_string(),
        },
        Architecture::Aarch64(_) => match trimmed.to_ascii_lowercase().as_str() {
            "lse" => "has_lse".to_string(),
            "pauth" | "paca" => "has_pauth".to_string(),
            "fp16" => "has_fp16".to_string(),
            "bti" => "use_bti".to_string(),
            other => other.to_string(),
        },
        _ => trimmed.to_string(),
    }
}

fn architecture_name(arch: Architecture) -> &'static str {
    match arch {
        Architecture::X86_64 => "x86_64",
        Architecture::Aarch64(_) => "aarch64",
        Architecture::Riscv64(_) => "riscv64",
        Architecture::S390x => "s390x",
        _ => "this architecture",
    }
}

fn current_host_triple() -> Triple {
    Triple::host()
}

// ── External runtime symbol table ─────────────────────────────────────────────

/// IDs for every external C-ABI runtime function we call from generated code.
#[allow(dead_code)]
struct RuntimeDecls {
    // Boxing / unboxing
    box_int: cranelift_module::FuncId,
    box_float: cranelift_module::FuncId,
    box_bool: cranelift_module::FuncId,
    box_handle: cranelift_module::FuncId,
    box_nothing: cranelift_module::FuncId,
    box_str: cranelift_module::FuncId,
    box_fn_ref: cranelift_module::FuncId,
    box_stdlib_fn: cranelift_module::FuncId,
    box_namespace: cranelift_module::FuncId,
    box_enum_type: cranelift_module::FuncId,
    box_class_type: cranelift_module::FuncId,
    make_shared: cranelift_module::FuncId,
    unbox_int: cranelift_module::FuncId,
    unbox_float: cranelift_module::FuncId,
    unbox_bool: cranelift_module::FuncId,
    unbox_handle: cranelift_module::FuncId,
    // Reference counting
    clone_any: cranelift_module::FuncId,
    drop_any: cranelift_module::FuncId,
    // Truthiness
    truthy: cranelift_module::FuncId,
    null_coalesce: cranelift_module::FuncId,
    // Dynamic arithmetic
    dyn_add: cranelift_module::FuncId,
    dyn_sub: cranelift_module::FuncId,
    dyn_mul: cranelift_module::FuncId,
    dyn_div: cranelift_module::FuncId,
    dyn_rem: cranelift_module::FuncId,
    dyn_pow: cranelift_module::FuncId,
    // Dynamic comparisons
    dyn_eq: cranelift_module::FuncId,
    dyn_ne: cranelift_module::FuncId,
    dyn_lt: cranelift_module::FuncId,
    dyn_le: cranelift_module::FuncId,
    dyn_gt: cranelift_module::FuncId,
    dyn_ge: cranelift_module::FuncId,
    // Dynamic logical / unary
    dyn_and: cranelift_module::FuncId,
    dyn_or: cranelift_module::FuncId,
    dyn_not: cranelift_module::FuncId,
    dyn_neg: cranelift_module::FuncId,
    // Bitwise
    dyn_bit_xor: cranelift_module::FuncId,
    dyn_bit_and: cranelift_module::FuncId,
    dyn_bit_or: cranelift_module::FuncId,
    dyn_shl: cranelift_module::FuncId,
    dyn_shr: cranelift_module::FuncId,
    // Range
    make_range: cranelift_module::FuncId,
    // Builtins
    println_fn: cranelift_module::FuncId,
    print_many_fn: cranelift_module::FuncId,
    input_fn: cranelift_module::FuncId,
    len_fn: cranelift_module::FuncId,
    panic_fn: cranelift_module::FuncId,
    assert_fn: cranelift_module::FuncId,
    type_name: cranelift_module::FuncId,
    to_string: cranelift_module::FuncId,
    to_integer: cranelift_module::FuncId,
    to_float: cranelift_module::FuncId,
    to_boolean: cranelift_module::FuncId,
    certain_check: cranelift_module::FuncId,
    // List / Dict / Object
    slice_fn: cranelift_module::FuncId,
    list_new: cranelift_module::FuncId,
    list_push: cranelift_module::FuncId,
    tuple_pack: cranelift_module::FuncId,
    list_get: cranelift_module::FuncId,
    list_set: cranelift_module::FuncId,
    list_len: cranelift_module::FuncId,
    dict_new: cranelift_module::FuncId,
    dict_get: cranelift_module::FuncId,
    dict_set: cranelift_module::FuncId,
    dict_len: cranelift_module::FuncId,
    dict_contains_key: cranelift_module::FuncId,
    dict_remove: cranelift_module::FuncId,
    dict_keys: cranelift_module::FuncId,
    dict_values: cranelift_module::FuncId,
    dict_entries: cranelift_module::FuncId,
    hashset_insert: cranelift_module::FuncId,
    hashset_remove: cranelift_module::FuncId,
    hashset_contains: cranelift_module::FuncId,
    hashset_to_list: cranelift_module::FuncId,
    hashset_union: cranelift_module::FuncId,
    hashset_intersect: cranelift_module::FuncId,
    hashset_diff: cranelift_module::FuncId,
    obj_new: cranelift_module::FuncId,
    obj_get_field: cranelift_module::FuncId,
    obj_set_field: cranelift_module::FuncId,
    obj_invoke: cranelift_module::FuncId,
    // Enum
    enum_variant: cranelift_module::FuncId,
    enum_tag_check: cranelift_module::FuncId,
    enum_payload: cranelift_module::FuncId,
    // Stdlib dispatch
    stdlib_call: cranelift_module::FuncId,
    // String interpolation
    str_interp: cranelift_module::FuncId,
    // Exception handling (AOT stubs)
    push_catch: cranelift_module::FuncId,
    pop_catch: cranelift_module::FuncId,
    throw_fn: cranelift_module::FuncId,
    throw_unhandled: cranelift_module::FuncId,
    store_exception: cranelift_module::FuncId,
    catch_exception: cranelift_module::FuncId,
    has_exception: cranelift_module::FuncId,
    // Closures
    make_closure: cranelift_module::FuncId,
    // Dynamic function dispatch table
    fn_table_init: cranelift_module::FuncId,
    fn_table_set: cranelift_module::FuncId,
    fn_name_register: cranelift_module::FuncId,
    call_dynamic: cranelift_module::FuncId,
    spawn_expr: cranelift_module::FuncId,
    spawn_dynamic: cranelift_module::FuncId,
    spawn_concurrent: cranelift_module::FuncId,
    spawn_task: cranelift_module::FuncId,
    pending_join: cranelift_module::FuncId,
    parallel_iter_seq: cranelift_module::FuncId,
}

impl RuntimeDecls {
    #[allow(unused_mut)]
    fn declare(module: &mut ObjectModule) -> Result<Self> {
        // Type aliases for readability.
        let p = cranelift_codegen::ir::types::I64; // opaque ptr (we use I64 as pointer proxy)
        let i64t = I64;
        let i8t = I8;
        let f64t = cranelift_codegen::ir::types::F64;
        let ext = Linkage::Import;

        macro_rules! decl {
            ($name:expr, $sig:expr) => {{
                let sig = $sig;
                module
                    .declare_function($name, ext, &sig)
                    .with_context(|| format!("declaring {}", $name))?
            }};
        }

        macro_rules! sig {
            (($($p:expr),*) -> ptr) => {{
                let mut s = module.make_signature();
                $(s.params.push(AbiParam::new($p));)*
                s.returns.push(AbiParam::new(I64)); s
            }};
            (($($p:expr),*) -> i64) => {{
                let mut s = module.make_signature();
                $(s.params.push(AbiParam::new($p));)*
                s.returns.push(AbiParam::new(I64)); s
            }};
            (($($p:expr),*) -> i8) => {{
                let mut s = module.make_signature();
                $(s.params.push(AbiParam::new($p));)*
                s.returns.push(AbiParam::new(I8)); s
            }};
            (($($p:expr),*) -> void) => {{
                let mut s = module.make_signature();
                $(s.params.push(AbiParam::new($p));)*
                s
            }};
        }

        Ok(RuntimeDecls {
            box_int: decl!("fdn_box_int", sig!((i64t) -> ptr)),
            box_float: decl!("fdn_box_float", sig!((f64t) -> ptr)),
            box_bool: decl!("fdn_box_bool", sig!((i8t) -> ptr)),
            box_handle: decl!("fdn_box_handle", sig!((i64t) -> ptr)),
            box_nothing: decl!("fdn_box_nothing", sig!(() -> ptr)),
            box_str: decl!("fdn_box_str", sig!((p, i64t) -> ptr)),
            box_fn_ref: decl!("fdn_box_fn_ref", sig!((i64t) -> ptr)),
            box_stdlib_fn: decl!("fdn_box_stdlib_fn", sig!((p, i64t, p, i64t) -> ptr)),
            box_namespace: decl!("fdn_box_namespace", sig!((p, i64t) -> ptr)),
            box_enum_type: decl!("fdn_box_enum_type", sig!((p, i64t) -> ptr)),
            box_class_type: decl!("fdn_box_class_type", sig!((p, i64t) -> ptr)),
            make_shared: decl!("fdn_make_shared", sig!((p) -> ptr)),
            unbox_int: decl!("fdn_unbox_int", sig!((p) -> i64)),
            unbox_float: decl!("fdn_unbox_float", {
                let mut s = module.make_signature();
                s.params.push(AbiParam::new(p));
                s.returns.push(AbiParam::new(F64));
                s
            }),
            unbox_bool: decl!("fdn_unbox_bool", sig!((p) -> i8)),
            unbox_handle: decl!("fdn_unbox_handle", sig!((p) -> i64)),
            clone_any: decl!("fdn_clone", sig!((p) -> ptr)),
            drop_any: decl!("fdn_drop", sig!((p) -> void)),
            truthy: decl!("fdn_truthy", sig!((p) -> i8)),
            null_coalesce: decl!("fdn_null_coalesce", sig!((p, p) -> ptr)),
            dyn_add: decl!("fdn_dyn_add", sig!((p, p) -> ptr)),
            dyn_sub: decl!("fdn_dyn_sub", sig!((p, p) -> ptr)),
            dyn_mul: decl!("fdn_dyn_mul", sig!((p, p) -> ptr)),
            dyn_div: decl!("fdn_dyn_div", sig!((p, p) -> ptr)),
            dyn_rem: decl!("fdn_dyn_rem", sig!((p, p) -> ptr)),
            dyn_pow: decl!("fdn_dyn_pow", sig!((p, p) -> ptr)),
            dyn_eq: decl!("fdn_dyn_eq", sig!((p, p) -> i8)),
            dyn_ne: decl!("fdn_dyn_ne", sig!((p, p) -> i8)),
            dyn_lt: decl!("fdn_dyn_lt", sig!((p, p) -> i8)),
            dyn_le: decl!("fdn_dyn_le", sig!((p, p) -> i8)),
            dyn_gt: decl!("fdn_dyn_gt", sig!((p, p) -> i8)),
            dyn_ge: decl!("fdn_dyn_ge", sig!((p, p) -> i8)),
            dyn_and: decl!("fdn_dyn_and", sig!((p, p) -> ptr)),
            dyn_or: decl!("fdn_dyn_or", sig!((p, p) -> ptr)),
            dyn_not: decl!("fdn_dyn_not", sig!((p) -> ptr)),
            dyn_neg: decl!("fdn_dyn_neg", sig!((p) -> ptr)),
            dyn_bit_xor: decl!("fdn_dyn_bit_xor", sig!((p, p) -> ptr)),
            dyn_bit_and: decl!("fdn_dyn_bit_and", sig!((p, p) -> ptr)),
            dyn_bit_or: decl!("fdn_dyn_bit_or", sig!((p, p) -> ptr)),
            dyn_shl: decl!("fdn_dyn_shl", sig!((p, p) -> ptr)),
            dyn_shr: decl!("fdn_dyn_shr", sig!((p, p) -> ptr)),
            make_range: decl!("fdn_make_range", {
                let mut s = module.make_signature();
                s.params.push(AbiParam::new(i64t));
                s.params.push(AbiParam::new(i64t));
                s.params.push(AbiParam::new(i8t));
                s.returns.push(AbiParam::new(I64));
                s
            }),
            println_fn: decl!("fdn_println", sig!((p) -> void)),
            print_many_fn: decl!("fdn_print_many", {
                let mut s = module.make_signature();
                s.params.push(AbiParam::new(I64)); // *const *mut FidanValue
                s.params.push(AbiParam::new(i64t)); // n
                s
            }),
            input_fn: decl!("fdn_input", sig!((p) -> ptr)),
            len_fn: decl!("fdn_len", sig!((p) -> i64)),
            panic_fn: decl!("fdn_panic", sig!((p) -> void)),
            assert_fn: decl!("fdn_assert", sig!((i64t, p) -> void)),
            type_name: decl!("fdn_type_name", sig!((p) -> ptr)),
            to_string: decl!("fdn_to_string", sig!((p) -> ptr)),
            to_integer: decl!("fdn_to_integer", sig!((p) -> ptr)),
            to_float: decl!("fdn_to_float", sig!((p) -> ptr)),
            to_boolean: decl!("fdn_to_boolean", sig!((p) -> ptr)),
            certain_check: decl!("fdn_certain_check", sig!((p, p, i64t) -> void)),
            slice_fn: decl!("fdn_slice", {
                let mut s = module.make_signature();
                s.params.push(AbiParam::new(I64)); // obj
                s.params.push(AbiParam::new(I64)); // start (*mut FidanValue or nothing)
                s.params.push(AbiParam::new(I64)); // end
                s.params.push(AbiParam::new(i8t)); // inclusive
                s.params.push(AbiParam::new(I64)); // step
                s.returns.push(AbiParam::new(I64));
                s
            }),
            list_new: decl!("fdn_list_new", sig!(() -> ptr)),
            list_push: decl!("fdn_list_push", sig!((p, p) -> void)),
            tuple_pack: decl!("fdn_tuple_pack", sig!((p, i64t) -> ptr)),
            list_get: decl!("fdn_list_get", sig!((p, p) -> ptr)),
            list_set: decl!("fdn_list_set", sig!((p, p, p) -> void)),
            list_len: decl!("fdn_list_len", sig!((p) -> i64)),
            dict_new: decl!("fdn_dict_new", sig!(() -> ptr)),
            dict_get: decl!("fdn_dict_get", sig!((p, p) -> ptr)),
            dict_set: decl!("fdn_dict_set", sig!((p, p, p) -> void)),
            dict_len: decl!("fdn_dict_len", sig!((p) -> i64)),
            dict_contains_key: decl!("fdn_dict_contains_key", sig!((p, p) -> i8)),
            dict_remove: decl!("fdn_dict_remove", sig!((p, p) -> void)),
            dict_keys: decl!("fdn_dict_keys", sig!((p) -> ptr)),
            dict_values: decl!("fdn_dict_values", sig!((p) -> ptr)),
            dict_entries: decl!("fdn_dict_entries", sig!((p) -> ptr)),
            hashset_insert: decl!("fdn_hashset_insert", sig!((p, p) -> void)),
            hashset_remove: decl!("fdn_hashset_remove", sig!((p, p) -> void)),
            hashset_contains: decl!("fdn_hashset_contains", sig!((p, p) -> i8)),
            hashset_to_list: decl!("fdn_hashset_to_list", sig!((p) -> ptr)),
            hashset_union: decl!("fdn_hashset_union", sig!((p, p) -> ptr)),
            hashset_intersect: decl!("fdn_hashset_intersect", sig!((p, p) -> ptr)),
            hashset_diff: decl!("fdn_hashset_diff", sig!((p, p) -> ptr)),
            obj_new: decl!("fdn_obj_new", sig!((p, i64t) -> ptr)),
            obj_get_field: decl!("fdn_obj_get_field", sig!((p, p, i64t) -> ptr)),
            obj_set_field: decl!("fdn_obj_set_field", sig!((p, p, i64t, p) -> void)),
            obj_invoke: decl!("fdn_obj_invoke", sig!((p, p, i64t, p, i64t) -> ptr)),
            enum_variant: decl!("fdn_enum_variant", sig!((p, i64t, p, i64t) -> ptr)),
            enum_tag_check: decl!("fdn_enum_tag_check", sig!((p, p, i64t) -> i8)),
            enum_payload: decl!("fdn_enum_payload", sig!((p, i64t) -> ptr)),
            stdlib_call: decl!("fdn_stdlib_call", sig!((p, i64t, p, i64t, p, i64t) -> ptr)),
            str_interp: decl!("fdn_str_interp", sig!((p, i64t) -> ptr)),
            push_catch: decl!("fdn_push_catch", sig!((i64t) -> void)),
            pop_catch: decl!("fdn_pop_catch", sig!(() -> void)),
            throw_fn: decl!("fdn_throw", sig!((p) -> void)),
            throw_unhandled: decl!("fdn_throw_unhandled", sig!((p) -> void)),
            store_exception: decl!("fdn_store_exception", sig!((p) -> void)),
            catch_exception: decl!("fdn_catch_exception", sig!(() -> ptr)),
            has_exception: decl!("fdn_has_exception", sig!(() -> i8)),
            make_closure: decl!("fdn_make_closure", sig!((i64t, p, i64t) -> ptr)),
            fn_table_init: decl!("fdn_fn_table_init", sig!((i64t) -> void)),
            fn_table_set: decl!("fdn_fn_table_set", sig!((i64t, i64t) -> void)),
            fn_name_register: decl!("fdn_fn_name_register", sig!((p, i64t, i64t) -> void)),
            call_dynamic: decl!("fdn_call_dynamic", sig!((p, p, i64t) -> ptr)),
            spawn_expr: decl!("fdn_spawn_expr", sig!((i64t, p, i64t) -> ptr)),
            spawn_dynamic: decl!("fdn_spawn_dynamic", sig!((p, p, i64t, p, i64t) -> ptr)),
            spawn_concurrent: decl!(
                "fdn_spawn_concurrent",
                sig!((i64t, p, i64t, p, i64t) -> ptr)
            ),
            spawn_task: decl!("fdn_spawn_task", sig!((i64t, p, i64t, p, i64t) -> ptr)),
            pending_join: decl!("fdn_pending_join", sig!((p) -> ptr)),
            parallel_iter_seq: decl!("fdn_parallel_iter_seq", sig!((p, i64t, p, i64t) -> void)),
        })
    }
}

// ── Low-level helpers ──────────────────────────────────────────────────────────

/// We represent all heap-allocated Fidan values as I64 in Cranelift
/// (native pointer width on 64-bit; zero-extends on 32-bit which we ignore).
const PTR_TY: cranelift_codegen::ir::Type = I64;

fn mir_ty_to_cl(ty: &MirTy) -> cranelift_codegen::ir::Type {
    match ty {
        MirTy::Integer => I64,
        MirTy::Float => F64,
        MirTy::Boolean => I8,
        MirTy::Handle => I64,
        _ => PTR_TY, // heap pointer
    }
}

fn is_scalar(ty: &MirTy) -> bool {
    matches!(
        ty,
        MirTy::Integer | MirTy::Float | MirTy::Boolean | MirTy::Handle
    )
}

fn native_extern_mir_ty_to_cl(ty: &MirTy) -> Result<cranelift_codegen::ir::Type> {
    match ty {
        MirTy::Integer | MirTy::Handle => Ok(I64),
        MirTy::Float => Ok(F64),
        MirTy::Boolean => Ok(I8),
        other => bail!("unsupported native @extern type in Cranelift backend: {other:?}"),
    }
}

// ── Function declaration ───────────────────────────────────────────────────────

fn declare_all_functions(
    module: &mut ObjectModule,
    program: &MirProgram,
    interner: &SymbolInterner,
) -> Result<Vec<cranelift_module::FuncId>> {
    let mut ids = Vec::with_capacity(program.functions.len());
    for mf in &program.functions {
        let name = interner.resolve(mf.name);
        let mangled = mangle_fn(name.as_ref(), mf.id.0);
        let mut sig = module.make_signature();
        for p in &mf.params {
            sig.params.push(AbiParam::new(mir_ty_to_cl(&p.ty)));
        }
        // If the declared return type is Nothing/Error but the body contains
        // a `return expr` terminator (e.g. an `action` that returns a value),
        // promote the Cranelift signature to return a PTR_TY (boxed Dynamic).
        // This ensures spawned calls and trampolines can capture the result.
        let effective_ret = effective_return_ty(mf);
        match effective_ret {
            MirTy::Nothing | MirTy::Error => {}
            rt => sig.returns.push(AbiParam::new(mir_ty_to_cl(&rt))),
        }
        let is_public = mangled == "fdn_main" || mangled == "fdn_init";
        let linkage = if is_public {
            Linkage::Export
        } else {
            Linkage::Local
        };
        let id = module
            .declare_function(&mangled, linkage, &sig)
            .with_context(|| format!("declaring fn {mangled}"))?;
        ids.push(id);
    }
    Ok(ids)
}

fn declare_all_extern_functions(
    module: &mut ObjectModule,
    program: &MirProgram,
) -> Result<HashMap<u32, cranelift_module::FuncId>> {
    let mut ids = HashMap::new();
    for mf in &program.functions {
        let Some(extern_decl) = &mf.extern_decl else {
            continue;
        };

        let mut sig = module.make_signature();
        match extern_decl.abi {
            MirExternAbi::Native => {
                for param in &mf.params {
                    sig.params
                        .push(AbiParam::new(native_extern_mir_ty_to_cl(&param.ty)?));
                }
                if !matches!(mf.return_ty, MirTy::Nothing | MirTy::Error) {
                    sig.returns
                        .push(AbiParam::new(native_extern_mir_ty_to_cl(&mf.return_ty)?));
                }
            }
            MirExternAbi::Fidan => {
                sig.params.push(AbiParam::new(PTR_TY));
                sig.params.push(AbiParam::new(I64));
                sig.returns.push(AbiParam::new(PTR_TY));
            }
        }

        let id = module
            .declare_function(&extern_decl.symbol, Linkage::Import, &sig)
            .with_context(|| format!("declaring extern symbol `{}`", extern_decl.symbol))?;
        ids.insert(mf.id.0, id);
    }
    Ok(ids)
}

fn collect_extern_link_inputs(program: &MirProgram) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut inputs = Vec::new();
    for function in &program.functions {
        let Some(extern_decl) = &function.extern_decl else {
            continue;
        };
        let raw = extern_decl
            .link
            .as_deref()
            .unwrap_or(extern_decl.lib.as_str())
            .trim();
        if raw.is_empty() || raw == "self" {
            continue;
        }
        if seen.insert(raw.to_owned()) {
            inputs.push(raw.to_owned());
        }
    }
    inputs
}

fn mangle_fn(name: &str, id: u32) -> String {
    if name == "main" {
        return "fdn_main".to_owned();
    }
    if name == "__init__" || id == 0 {
        return "fdn_init".to_owned();
    }
    format!("fdn_{}_{}", name, id)
}

/// The effective Cranelift return type for a MirFunction.
///
/// When the declared `return_ty` is `Nothing` or `Error` but the function body
/// contains at least one `Terminator::Return(Some(_))`, the function actually
/// returns a value at runtime (e.g. `action foo { return x * 2 }`).  We promote
/// such functions to return `Dynamic` (PTR_TY) so that spawned calls and
/// trampolines can capture the boxed result.
fn effective_return_ty(mf: &MirFunction) -> MirTy {
    match &mf.return_ty {
        MirTy::Nothing | MirTy::Error => {
            let has_value_return = mf
                .blocks
                .iter()
                .any(|bb| matches!(&bb.terminator, Terminator::Return(Some(_))));
            if has_value_return {
                MirTy::Dynamic
            } else {
                mf.return_ty.clone()
            }
        }
        other => other.clone(),
    }
}

// ── Per-function state ─────────────────────────────────────────────────────────

#[allow(dead_code)]
struct FnState {
    /// LocalId.0 → Cranelift Variable index (same index)
    num_locals: usize,
    local_types: HashMap<u32, MirTy>,
    /// BlockId.0 → Cranelift Block
    cl_blocks: Vec<cranelift_codegen::ir::Block>,
    /// (block_idx, phi_idx) → Cranelift block param value (for non-entry blocks)
    phi_param_vals: HashMap<(usize, usize), cranelift_codegen::ir::Value>,
}

// ── Catch-stack pre-pass ───────────────────────────────────────────────────────
//
// Computes the catch-handler stack state at the ENTRY of each basic block.
// This lets `Terminator::Throw` jump directly to the correct catch block
// without any runtime indirection.

fn compute_catch_stacks(mf: &MirFunction) -> Vec<Vec<MirBlockId>> {
    let n = mf.blocks.len();
    let mut entry_stacks: Vec<Option<Vec<MirBlockId>>> = vec![None; n];
    entry_stacks[0] = Some(Vec::new());

    let mut worklist = std::collections::VecDeque::new();
    worklist.push_back(0usize);

    while let Some(bi) = worklist.pop_front() {
        let Some(entry_stack) = entry_stacks[bi].clone() else {
            continue;
        };
        let mut state = entry_stack;

        // Apply this block's PushCatch / PopCatch instructions.
        for instr in &mf.blocks[bi].instructions {
            match instr {
                Instr::PushCatch(target) => state.push(*target),
                Instr::PopCatch => {
                    state.pop();
                }
                _ => {}
            }
        }

        // Propagate to successors (use first-reaching state for merge points).
        let propagate = |dst: usize,
                         st: Vec<MirBlockId>,
                         stacks: &mut Vec<Option<Vec<MirBlockId>>>,
                         wl: &mut std::collections::VecDeque<usize>| {
            if stacks[dst].is_none() {
                stacks[dst] = Some(st);
                wl.push_back(dst);
            }
        };

        match &mf.blocks[bi].terminator {
            Terminator::Goto(t) => {
                let idx = t.0 as usize;
                propagate(idx, state, &mut entry_stacks, &mut worklist);
            }
            Terminator::Branch {
                then_bb, else_bb, ..
            } => {
                let ti = then_bb.0 as usize;
                let ei = else_bb.0 as usize;
                propagate(ti, state.clone(), &mut entry_stacks, &mut worklist);
                propagate(ei, state, &mut entry_stacks, &mut worklist);
            }
            Terminator::Throw { .. } => {
                // Throw pops the top catch block and jumps to it.
                // The catch block's entry state is everything below the top.
                if let Some(catch_bid) = state.last().copied() {
                    let mut after_pop = state.clone();
                    after_pop.pop();
                    let idx = catch_bid.0 as usize;
                    propagate(idx, after_pop, &mut entry_stacks, &mut worklist);
                }
            }
            _ => {}
        }
    }

    entry_stacks
        .into_iter()
        .map(|s| s.unwrap_or_default())
        .collect()
}

// ── Main function lowering ─────────────────────────────────────────────────────

fn lower_extern_wrapper(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    _fn_ids: &[cranelift_module::FuncId],
    extern_fn_ids: &HashMap<u32, cranelift_module::FuncId>,
    ctx: &mut Context,
    builder_ctx: &mut FunctionBuilderContext,
    mf: &MirFunction,
) -> Result<()> {
    let extern_decl = mf
        .extern_decl
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing extern metadata for wrapper {}", mf.id.0))?;

    let mut sig = module.make_signature();
    for param in &mf.params {
        let cl_ty = match extern_decl.abi {
            MirExternAbi::Native => native_extern_mir_ty_to_cl(&param.ty)?,
            MirExternAbi::Fidan => mir_ty_to_cl(&param.ty),
        };
        sig.params.push(AbiParam::new(cl_ty));
    }
    if !matches!(mf.return_ty, MirTy::Nothing | MirTy::Error) {
        let cl_ty = match extern_decl.abi {
            MirExternAbi::Native => native_extern_mir_ty_to_cl(&mf.return_ty)?,
            MirExternAbi::Fidan => mir_ty_to_cl(&mf.return_ty),
        };
        sig.returns.push(AbiParam::new(cl_ty));
    }

    let wrapper_name = mangle_fn("__extern_wrapper", mf.id.0);
    ctx.func = Function::with_name_signature(UserFuncName::testcase(wrapper_name.as_str()), sig);

    let mut builder = FunctionBuilder::new(&mut ctx.func, builder_ctx);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);

    let imported_id = extern_fn_ids
        .get(&mf.id.0)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("missing imported extern function {}", mf.id.0))?;
    let imported_ref = module.declare_func_in_func(imported_id, builder.func);
    let params = builder.block_params(entry).to_vec();

    match extern_decl.abi {
        MirExternAbi::Native => {
            let call = builder.ins().call(imported_ref, &params);
            let results = builder.inst_results(call).to_vec();
            if results.is_empty() {
                builder.ins().return_(&[]);
            } else {
                builder.ins().return_(&[results[0]]);
            }
        }
        MirExternAbi::Fidan => {
            let boxed_args = box_fidan_abi_params(module, rt, &mut builder, mf, &params)?;
            let (args_ptr, args_cnt) = build_boxed_value_array(&mut builder, &boxed_args)?;
            let call = builder.ins().call(imported_ref, &[args_ptr, args_cnt]);
            let raw_result = builder.inst_results(call)[0];

            for boxed in &boxed_args {
                call_rt(module, &mut builder, rt.drop_any, &[*boxed])?;
            }

            let result_ptr = coalesce_null_ptr_to_nothing(module, rt, &mut builder, raw_result)?;
            if matches!(mf.return_ty, MirTy::Nothing | MirTy::Error) {
                call_rt(module, &mut builder, rt.drop_any, &[result_ptr])?;
                builder.ins().return_(&[]);
            } else {
                let value =
                    unbox_for_declared_type(module, rt, &mut builder, result_ptr, &mf.return_ty)?;
                if matches!(
                    mf.return_ty,
                    MirTy::Integer | MirTy::Float | MirTy::Boolean | MirTy::Handle
                ) {
                    call_rt(module, &mut builder, rt.drop_any, &[result_ptr])?;
                }
                builder.ins().return_(&[value]);
            }
        }
    }

    builder.finalize();

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn lower_function(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    fn_ids: &[cranelift_module::FuncId],
    extern_fn_ids: &HashMap<u32, cranelift_module::FuncId>,
    global_data_ids: &[cranelift_module::DataId],
    function_throw_map: &HashMap<MirFunctionId, bool>,
    ctx: &mut Context,
    builder_ctx: &mut FunctionBuilderContext,
    program: &MirProgram,
    mf: &MirFunction,
    interner: &SymbolInterner,
) -> Result<()> {
    if mf.extern_decl.is_some() {
        return lower_extern_wrapper(module, rt, fn_ids, extern_fn_ids, ctx, builder_ctx, mf);
    }

    let global_ns_map = build_global_namespace_map(program, interner);

    // Build the Cranelift function signature.
    let mut sig = module.make_signature();
    for p in &mf.params {
        sig.params.push(AbiParam::new(mir_ty_to_cl(&p.ty)));
    }
    let eff_ret = effective_return_ty(mf);
    let has_return = !matches!(&eff_ret, MirTy::Nothing | MirTy::Error);
    if has_return {
        sig.returns.push(AbiParam::new(mir_ty_to_cl(&eff_ret)));
    }
    ctx.func = Function::with_name_signature(
        UserFuncName::testcase(mangle_fn(interner.resolve(mf.name).as_ref(), mf.id.0).as_str()),
        sig,
    );

    // Build a local-type map for operand lowering.
    let local_types = build_local_type_map(mf, program, interner);
    let num_locals = mf.local_count as usize;

    let mut builder = FunctionBuilder::new(&mut ctx.func, builder_ctx);

    // ── Phase 0: create one Cranelift block per MIR basic block ───────────────
    let cl_blocks: Vec<cranelift_codegen::ir::Block> =
        mf.blocks.iter().map(|_| builder.create_block()).collect();

    // Guard: a MirFunction with no basic blocks indicates a compiler bug
    // (most commonly: two top-level actions with the same name where the first
    // pre-allocation never got its body lowered).  Return a clean error instead
    // of panicking with an opaque index-out-of-bounds.
    if cl_blocks.is_empty() {
        anyhow::bail!(
            "function `{}` (id {:?}) has no basic blocks — \
            this is a compiler bug; please report it",
            interner.resolve(mf.name),
            mf.id
        );
    }

    // ── Phase 0b: block params for phi nodes (non-entry blocks) ───────────────
    let mut phi_param_vals: HashMap<(usize, usize), cranelift_codegen::ir::Value> = HashMap::new();
    builder.append_block_params_for_function_params(cl_blocks[0]);
    for (bi, mir_bb) in mf.blocks.iter().enumerate() {
        if bi == 0 {
            continue; // entry block uses function params
        }
        for (pi, phi) in mir_bb.phis.iter().enumerate() {
            let ty = local_types
                .get(&phi.result.0)
                .map(mir_ty_to_cl)
                .unwrap_or(PTR_TY);
            let v = builder.append_block_param(cl_blocks[bi], ty);
            phi_param_vals.insert((bi, pi), v);
        }
    }

    // Declare Cranelift Variables for all locals.
    let mut cl_vars: Vec<Variable> = Vec::with_capacity(num_locals);
    for i in 0..num_locals {
        let ty = local_types
            .get(&(i as u32))
            .map(mir_ty_to_cl)
            .unwrap_or(PTR_TY);
        let var = builder.declare_var(ty);
        cl_vars.push(var);
    }

    // ── Entry block: bind function params to local variables ──────────────────
    builder.switch_to_block(cl_blocks[0]);
    {
        let params: Vec<cranelift_codegen::ir::Value> = builder.block_params(cl_blocks[0]).to_vec();
        for (idx, param) in mf.params.iter().enumerate() {
            builder.def_var(cl_vars[param.local.0 as usize], params[idx]);
        }
    }

    // ── Catch-stack pre-pass ───────────────────────────────────────────────────
    // Compute the catch-handler stack state at the entry of each basic block.
    let entry_catch_stacks = compute_catch_stacks(mf);

    // ── Lower each basic block ─────────────────────────────────────────────────
    for (bi, mir_bb) in mf.blocks.iter().enumerate() {
        let mut namespace_locals: HashMap<LocalId, String> = HashMap::new();

        if bi > 0 {
            builder.switch_to_block(cl_blocks[bi]);
            for (pi, phi) in mir_bb.phis.iter().enumerate() {
                if let Some(&v) = phi_param_vals.get(&(bi, pi)) {
                    builder.def_var(cl_vars[phi.result.0 as usize], v);
                }
            }
        }

        // Track the current catch stack during instruction processing.
        let mut current_catch_stack = entry_catch_stacks[bi].clone();

        for instr in &mir_bb.instructions {
            // PushCatch / PopCatch update the compile-time catch state only.
            match instr {
                Instr::PushCatch(target) => {
                    current_catch_stack.push(*target);
                    continue;
                }
                Instr::PopCatch => {
                    current_catch_stack.pop();
                    continue;
                }
                _ => {}
            }
            lower_instr(
                module,
                rt,
                fn_ids,
                global_data_ids,
                function_throw_map,
                &mut builder,
                &cl_vars,
                &local_types,
                &global_ns_map,
                &mut namespace_locals,
                program,
                mf,
                bi,
                &cl_blocks,
                &current_catch_stack,
                instr,
                interner,
            )?;
        }

        lower_terminator(
            &mut builder,
            &cl_blocks,
            &cl_vars,
            &local_types,
            mf,
            bi,
            &mir_bb.terminator,
            &current_catch_stack,
            rt,
            module,
            fn_ids,
            program,
            interner,
        )?;
    }

    builder.seal_all_blocks();
    builder.finalize();
    Ok(())
}

fn box_fidan_abi_params(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    builder: &mut FunctionBuilder<'_>,
    mf: &MirFunction,
    params: &[cranelift_codegen::ir::Value],
) -> Result<Vec<cranelift_codegen::ir::Value>> {
    let mut boxed = Vec::with_capacity(params.len());
    for (param, value) in mf.params.iter().zip(params.iter().copied()) {
        boxed.push(box_raw_value_for_type(
            module, rt, builder, value, &param.ty,
        )?);
    }
    Ok(boxed)
}

fn box_raw_value_for_type(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    builder: &mut FunctionBuilder<'_>,
    value: cranelift_codegen::ir::Value,
    ty: &MirTy,
) -> Result<cranelift_codegen::ir::Value> {
    match ty {
        MirTy::Integer => Ok(call_rt(module, builder, rt.box_int, &[value])?.unwrap()),
        MirTy::Float => Ok(call_rt(module, builder, rt.box_float, &[value])?.unwrap()),
        MirTy::Boolean => Ok(call_rt(module, builder, rt.box_bool, &[value])?.unwrap()),
        MirTy::Handle => Ok(call_rt(module, builder, rt.box_handle, &[value])?.unwrap()),
        _ => Ok(call_rt(module, builder, rt.clone_any, &[value])?.unwrap_or(value)),
    }
}

fn build_boxed_value_array(
    builder: &mut FunctionBuilder<'_>,
    values: &[cranelift_codegen::ir::Value],
) -> Result<(cranelift_codegen::ir::Value, cranelift_codegen::ir::Value)> {
    if values.is_empty() {
        return Ok((
            builder.ins().iconst(PTR_TY, 0),
            builder.ins().iconst(I64, 0),
        ));
    }

    let slot = builder.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
        cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
        (values.len() * 8) as u32,
        3u8,
    ));
    for (index, value) in values.iter().enumerate() {
        builder.ins().stack_store(*value, slot, (index as i32) * 8);
    }
    let ptr = builder.ins().stack_addr(PTR_TY, slot, 0);
    let cnt = builder.ins().iconst(I64, values.len() as i64);
    Ok((ptr, cnt))
}

fn coalesce_null_ptr_to_nothing(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    builder: &mut FunctionBuilder<'_>,
    value: cranelift_codegen::ir::Value,
) -> Result<cranelift_codegen::ir::Value> {
    let is_null = builder.ins().icmp_imm(IntCC::Equal, value, 0);
    let nothing = call_rt(module, builder, rt.box_nothing, &[])?.unwrap();
    Ok(builder.ins().select(is_null, nothing, value))
}

fn unbox_for_declared_type(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    builder: &mut FunctionBuilder<'_>,
    value: cranelift_codegen::ir::Value,
    ty: &MirTy,
) -> Result<cranelift_codegen::ir::Value> {
    match ty {
        MirTy::Integer => Ok(call_rt(module, builder, rt.unbox_int, &[value])?.unwrap()),
        MirTy::Float => Ok(call_rt(module, builder, rt.unbox_float, &[value])?.unwrap()),
        MirTy::Boolean => Ok(call_rt(module, builder, rt.unbox_bool, &[value])?.unwrap()),
        MirTy::Handle => Ok(call_rt(module, builder, rt.unbox_handle, &[value])?.unwrap()),
        _ => Ok(value),
    }
}

// ── Instruction lowering ───────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn lower_instr(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    fn_ids: &[cranelift_module::FuncId],
    global_data_ids: &[cranelift_module::DataId],
    function_throw_map: &HashMap<MirFunctionId, bool>,
    builder: &mut FunctionBuilder<'_>,
    cl_vars: &[Variable],
    local_types: &HashMap<u32, MirTy>,
    global_ns_map: &HashMap<GlobalId, String>,
    namespace_locals: &mut HashMap<LocalId, String>,
    program: &MirProgram,
    mf: &MirFunction,
    bi: usize,
    cl_blocks: &[cranelift_codegen::ir::Block],
    current_catch_stack: &[MirBlockId],
    instr: &Instr,
    interner: &SymbolInterner,
) -> Result<()> {
    match instr {
        Instr::Assign { dest, ty, rhs } => {
            let effective_ty = local_types.get(&dest.0).unwrap_or(ty);
            let val = lower_rvalue(
                module,
                rt,
                fn_ids,
                builder,
                cl_vars,
                local_types,
                namespace_locals,
                program,
                rhs,
                effective_ty,
                interner,
            )?;
            // Cranelift requires that the type of the assigned value exactly matches
            // the declared type of the variable.  When dynamic dispatch is used (e.g.
            // a float op where one operand was loaded from a global and is therefore
            // `MirTy::Dynamic`), `lower_rvalue` may return a PTR_TY (I64) boxed
            // pointer even though the logical destination is scalar.  The local-type map
            // provides the backend's best-known effective destination type here.
            let expected_cl_ty = mir_ty_to_cl(effective_ty);
            let actual_cl_ty = builder.func.dfg.value_type(val);
            let val = if actual_cl_ty != expected_cl_ty {
                coerce_value(builder, module, rt, val, actual_cl_ty, expected_cl_ty)?
            } else {
                val
            };
            builder.def_var(cl_vars[dest.0 as usize], val);
            if let Rvalue::Call { callee, args } = rhs
                && call_may_throw(
                    callee,
                    args,
                    local_types,
                    namespace_locals,
                    function_throw_map,
                    interner,
                )?
            {
                emit_pending_exception_check(
                    module,
                    rt,
                    builder,
                    cl_blocks,
                    cl_vars,
                    local_types,
                    mf,
                    bi,
                    current_catch_stack,
                    interner,
                )?;
            }
        }

        Instr::Call {
            dest,
            result_ty,
            callee,
            args,
            ..
        } => {
            let call_result_ty = dest
                .and_then(|d| {
                    result_ty
                        .clone()
                        .filter(|ty| !matches!(ty, MirTy::Dynamic | MirTy::Error))
                        .or_else(|| local_types.get(&d.0).cloned())
                })
                .unwrap_or(MirTy::Dynamic);
            let ret = emit_call(
                module,
                rt,
                fn_ids,
                builder,
                cl_vars,
                local_types,
                namespace_locals,
                program,
                callee,
                args,
                &call_result_ty,
                interner,
            )?;
            if let (Some(d), Some(v)) = (dest, ret) {
                let effective_ty = result_ty
                    .clone()
                    .filter(|ty| !matches!(ty, MirTy::Dynamic | MirTy::Error))
                    .or_else(|| local_types.get(&d.0).cloned())
                    .unwrap_or(MirTy::Dynamic);
                let expected_cl_ty = mir_ty_to_cl(&effective_ty);
                let actual_cl_ty = builder.func.dfg.value_type(v);
                let v = if actual_cl_ty != expected_cl_ty {
                    let coerced =
                        coerce_value(builder, module, rt, v, actual_cl_ty, expected_cl_ty)?;
                    if actual_cl_ty == PTR_TY && is_scalar(&effective_ty) {
                        call_rt(module, builder, rt.drop_any, &[v])?;
                    }
                    coerced
                } else {
                    v
                };
                builder.def_var(cl_vars[d.0 as usize], v);
            }
            if call_may_throw(
                callee,
                args,
                local_types,
                namespace_locals,
                function_throw_map,
                interner,
            )? {
                emit_pending_exception_check(
                    module,
                    rt,
                    builder,
                    cl_blocks,
                    cl_vars,
                    local_types,
                    mf,
                    bi,
                    current_catch_stack,
                    interner,
                )?;
            }
        }

        Instr::GetField {
            dest,
            object,
            field,
        } => {
            let field_name = interner.resolve(*field);
            let stdlib_namespace = match object {
                Operand::Local(local) => namespace_locals.get(local).map(String::as_str),
                Operand::Const(MirLit::Namespace(namespace)) => Some(namespace.as_str()),
                _ => None,
            };

            let r = if let Some(namespace) = stdlib_namespace
                && module_exports(namespace).contains(&field_name.as_ref())
            {
                let (module_ptr, module_len) = str_const(module, builder, namespace)?;
                let (field_ptr, field_len) = str_const(module, builder, field_name.as_ref())?;
                call_rt(
                    module,
                    builder,
                    rt.box_stdlib_fn,
                    &[module_ptr, module_len, field_ptr, field_len],
                )?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0))
            } else {
                let obj = lower_operand_as_ptr(builder, cl_vars, local_types, object, rt, module)?;
                let (field_ptr, field_len) = str_const(module, builder, field_name.as_ref())?;
                call_rt(
                    module,
                    builder,
                    rt.obj_get_field,
                    &[obj, field_ptr, field_len],
                )?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0))
            };
            builder.def_var(cl_vars[dest.0 as usize], r);
        }

        Instr::SetField {
            object,
            field,
            value,
        } => {
            let obj = lower_operand_as_ptr(builder, cl_vars, local_types, object, rt, module)?;
            let val = lower_operand_boxed(builder, cl_vars, local_types, value, rt, module)?;
            let (fp, fl) = str_const(module, builder, interner.resolve(*field).as_ref())?;
            call_rt(module, builder, rt.obj_set_field, &[obj, fp, fl, val])?;
        }

        Instr::GetIndex {
            dest,
            object,
            index,
        } => {
            let obj = lower_operand_as_ptr(builder, cl_vars, local_types, object, rt, module)?;
            let idx = lower_operand_boxed(builder, cl_vars, local_types, index, rt, module)?;
            let access = match operand_mir_ty(local_types, object) {
                MirTy::Dict(_, _) => rt.dict_get,
                _ => rt.list_get,
            };
            let r = call_rt(module, builder, access, &[obj, idx])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
            builder.def_var(cl_vars[dest.0 as usize], r);
        }

        Instr::SetIndex {
            object,
            index,
            value,
        } => {
            let obj = lower_operand_as_ptr(builder, cl_vars, local_types, object, rt, module)?;
            let idx = lower_operand_boxed(builder, cl_vars, local_types, index, rt, module)?;
            let val = lower_operand_boxed(builder, cl_vars, local_types, value, rt, module)?;
            let access = match operand_mir_ty(local_types, object) {
                MirTy::Dict(_, _) => rt.dict_set,
                _ => rt.list_set,
            };
            call_rt(module, builder, access, &[obj, idx, val])?;
        }

        Instr::Drop { local } => {
            if let Some(ty) = local_types.get(&local.0)
                && !is_scalar(ty)
                && !matches!(ty, MirTy::Nothing | MirTy::Error)
            {
                let v = builder.use_var(cl_vars[local.0 as usize]);
                call_rt(module, builder, rt.drop_any, &[v])?;
            }
            let zero = zero_value_for_local(builder, local_types.get(&local.0));
            builder.def_var(cl_vars[local.0 as usize], zero);
        }

        Instr::CertainCheck { operand, name } => {
            let operand_ty = operand_mir_ty(local_types, operand);
            if !matches!(operand_ty, MirTy::Dynamic | MirTy::Error | MirTy::Nothing) {
                return Ok(());
            }
            let val = lower_operand_boxed(builder, cl_vars, local_types, operand, rt, module)?;
            let (np, nl) = str_const(module, builder, interner.resolve(*name).as_ref())?;
            call_rt(module, builder, rt.certain_check, &[val, np, nl])?;
        }

        Instr::PushCatch(bid) => {
            let id = builder.ins().iconst(I64, bid.0 as i64);
            call_rt(module, builder, rt.push_catch, &[id])?;
        }

        Instr::PopCatch => {
            call_rt(module, builder, rt.pop_catch, &[])?;
        }

        Instr::LoadGlobal { dest, global } => {
            let GlobalId(gid) = global;
            if let Some(&data_id) = global_data_ids.get(*gid as usize) {
                if let Some(ns) = global_ns_map.get(global) {
                    namespace_locals.insert(*dest, ns.clone());
                } else {
                    namespace_locals.remove(dest);
                }
                let gv = module.declare_data_in_func(data_id, builder.func);
                let addr = builder.ins().global_value(PTR_TY, gv);
                let val = builder.ins().load(PTR_TY, MemFlags::new(), addr, 0);
                store_local_from_boxed(builder, cl_vars, local_types, *dest, val, rt, module)?;
            } else {
                namespace_locals.remove(dest);
                let zero = zero_value_for_local(builder, local_types.get(&dest.0));
                builder.def_var(cl_vars[dest.0 as usize], zero);
            }
        }

        Instr::StoreGlobal { global, value } => {
            let GlobalId(gid) = global;
            if let Some(&data_id) = global_data_ids.get(*gid as usize) {
                let gv = module.declare_data_in_func(data_id, builder.func);
                let addr = builder.ins().global_value(PTR_TY, gv);
                let val = lower_operand_boxed(builder, cl_vars, local_types, value, rt, module)?;
                let cloned = call_rt(module, builder, rt.clone_any, &[val])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
                builder.ins().store(MemFlags::new(), cloned, addr, 0);
            }
        }

        // ── Concurrency / Parallelism ─────────────────────────────────────
        Instr::SpawnExpr {
            dest,
            task_fn,
            args,
        } => {
            let fn_idx = builder.ins().iconst(I64, task_fn.0 as i64);
            let (arr, cnt) =
                build_ptr_array(module, rt, builder, cl_vars, local_types, args, interner)?;
            let pending = call_rt(module, builder, rt.spawn_expr, &[fn_idx, arr, cnt])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
            builder.def_var(cl_vars[dest.0 as usize], pending);
        }

        Instr::SpawnConcurrent {
            handle: dest,
            task_fn,
            args,
        } => {
            let fn_idx = builder.ins().iconst(I64, task_fn.0 as i64);
            let task_name_sym = interner.resolve(program.function(*task_fn).name);
            let (name_ptr, name_len) = str_const(module, builder, task_name_sym.as_ref())?;
            let (arr, cnt) =
                build_ptr_array(module, rt, builder, cl_vars, local_types, args, interner)?;
            let pending = call_rt(
                module,
                builder,
                rt.spawn_concurrent,
                &[fn_idx, name_ptr, name_len, arr, cnt],
            )?
            .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
            builder.def_var(cl_vars[dest.0 as usize], pending);
        }

        Instr::SpawnParallel {
            handle: dest,
            task_fn,
            args,
        } => {
            let fn_idx = builder.ins().iconst(I64, task_fn.0 as i64);
            let task_name_sym = interner.resolve(program.function(*task_fn).name);
            let (name_ptr, name_len) = str_const(module, builder, task_name_sym.as_ref())?;
            let (arr, cnt) =
                build_ptr_array(module, rt, builder, cl_vars, local_types, args, interner)?;
            let pending = call_rt(
                module,
                builder,
                rt.spawn_task,
                &[fn_idx, name_ptr, name_len, arr, cnt],
            )?
            .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
            builder.def_var(cl_vars[dest.0 as usize], pending);
        }

        Instr::JoinAll { handles } => {
            for handle in handles {
                let handle_ptr = lower_operand_as_ptr(
                    builder,
                    cl_vars,
                    local_types,
                    &Operand::Local(*handle),
                    rt,
                    module,
                )?;
                let resolved = call_rt(module, builder, rt.pending_join, &[handle_ptr])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
                builder.def_var(cl_vars[handle.0 as usize], resolved);
                emit_pending_exception_check(
                    module,
                    rt,
                    builder,
                    cl_blocks,
                    cl_vars,
                    local_types,
                    mf,
                    bi,
                    current_catch_stack,
                    interner,
                )?;
            }
        }

        Instr::AwaitPending { dest, handle } => {
            let handle_ptr =
                lower_operand_as_ptr(builder, cl_vars, local_types, handle, rt, module)?;
            let resolved = call_rt(module, builder, rt.pending_join, &[handle_ptr])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
            builder.def_var(cl_vars[dest.0 as usize], resolved);
            emit_pending_exception_check(
                module,
                rt,
                builder,
                cl_blocks,
                cl_vars,
                local_types,
                mf,
                bi,
                current_catch_stack,
                interner,
            )?;
        }

        Instr::SpawnDynamic { dest, method, args } => {
            let (first, rest, method_ptr, method_len) = if let Some(sym) = method {
                let recv =
                    lower_operand_as_ptr(builder, cl_vars, local_types, &args[0], rt, module)?;
                let (mp, ml) = str_const(module, builder, interner.resolve(*sym).as_ref())?;
                (recv, &args[1..], mp, ml)
            } else {
                let fn_val =
                    lower_operand_boxed(builder, cl_vars, local_types, &args[0], rt, module)?;
                (
                    fn_val,
                    &args[1..],
                    builder.ins().iconst(PTR_TY, 0),
                    builder.ins().iconst(I64, 0),
                )
            };
            let (arr, cnt) =
                build_ptr_array(module, rt, builder, cl_vars, local_types, rest, interner)?;
            let pending = call_rt(
                module,
                builder,
                rt.spawn_dynamic,
                &[first, method_ptr, method_len, arr, cnt],
            )?
            .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
            builder.def_var(cl_vars[dest.0 as usize], pending);
        }

        Instr::ParallelIter {
            collection,
            body_fn,
            closure_args,
        } => {
            // Runtime implementation of `parallel for`: call fdn_parallel_iter_seq(collection, fn_idx, env, n).
            let coll = lower_operand_as_ptr(builder, cl_vars, local_types, collection, rt, module)?;
            let fn_idx = builder.ins().iconst(I64, body_fn.0 as i64);
            let (env_arr, env_cnt) = if closure_args.is_empty() {
                (
                    builder.ins().iconst(PTR_TY, 0),
                    builder.ins().iconst(I64, 0),
                )
            } else {
                let (p, n) = build_ptr_array(
                    module,
                    rt,
                    builder,
                    cl_vars,
                    local_types,
                    closure_args,
                    interner,
                )?;
                (p, n)
            };
            call_rt(
                module,
                builder,
                rt.parallel_iter_seq,
                &[coll, fn_idx, env_arr, env_cnt],
            )?;
            emit_pending_exception_check(
                module,
                rt,
                builder,
                cl_blocks,
                cl_vars,
                local_types,
                mf,
                bi,
                current_catch_stack,
                interner,
            )?;
        }

        Instr::Nop => {}
    }
    Ok(())
}

fn emit_exceptional_return(builder: &mut FunctionBuilder<'_>, mf: &MirFunction) {
    let eff_ty = effective_return_ty(mf);
    if matches!(&eff_ty, MirTy::Nothing | MirTy::Error) {
        builder.ins().return_(&[]);
        return;
    }

    let ret_cl_ty = mir_ty_to_cl(&eff_ty);
    let default_ret = if ret_cl_ty == F64 {
        builder.ins().f64const(0.0)
    } else {
        builder.ins().iconst(ret_cl_ty, 0)
    };
    builder.ins().return_(&[default_ret]);
}

#[allow(clippy::too_many_arguments)]
fn emit_pending_exception_check(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    builder: &mut FunctionBuilder<'_>,
    cl_blocks: &[cranelift_codegen::ir::Block],
    cl_vars: &[Variable],
    local_types: &HashMap<u32, MirTy>,
    mf: &MirFunction,
    bi: usize,
    current_catch_stack: &[MirBlockId],
    interner: &SymbolInterner,
) -> Result<()> {
    let has_exception = call_rt(module, builder, rt.has_exception, &[])?
        .unwrap_or_else(|| builder.ins().iconst(I8, 0));
    let pending_block = builder.create_block();
    let cont_block = builder.create_block();
    builder
        .ins()
        .brif(has_exception, pending_block, &[], cont_block, &[]);

    builder.switch_to_block(pending_block);
    if let Some(catch_bid) = current_catch_stack.last() {
        let catch_idx = catch_bid.0 as usize;
        let catch_args = collect_phi_args(
            module,
            rt,
            builder,
            cl_vars,
            local_types,
            mf,
            bi,
            catch_idx,
            interner,
        )?;
        builder.ins().jump(cl_blocks[catch_idx], &catch_args);
    } else {
        emit_exceptional_return(builder, mf);
    }

    builder.switch_to_block(cont_block);
    Ok(())
}
// ── Terminator lowering ────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn lower_terminator(
    builder: &mut FunctionBuilder<'_>,
    cl_blocks: &[cranelift_codegen::ir::Block],
    cl_vars: &[Variable],
    local_types: &HashMap<u32, MirTy>,
    mf: &MirFunction,
    bi: usize,
    term: &Terminator,
    current_catch_stack: &[MirBlockId],
    rt: &RuntimeDecls,
    module: &mut ObjectModule,
    _fn_ids: &[cranelift_module::FuncId],
    _program: &MirProgram,
    _interner: &SymbolInterner,
) -> Result<()> {
    match term {
        Terminator::Return(None) => {
            let eff_ty = effective_return_ty(mf);
            let has_return = !matches!(&eff_ty, MirTy::Nothing | MirTy::Error);
            if has_return {
                // Lambda/function body has no explicit return expression but the
                // MIR declared a return type.  Emit a zero / null placeholder so
                // the Cranelift verifier does not reject the function.
                let ret_cl_ty = mir_ty_to_cl(&eff_ty);
                let zero = if ret_cl_ty == F64 {
                    builder.ins().f64const(0.0)
                } else {
                    builder.ins().iconst(ret_cl_ty, 0)
                };
                builder.ins().return_(&[zero]);
            } else {
                builder.ins().return_(&[]);
            }
        }
        Terminator::Return(Some(op)) => {
            let eff_ty = effective_return_ty(mf);
            let has_return = !matches!(&eff_ty, MirTy::Nothing | MirTy::Error);
            if has_return {
                let v = if !is_scalar(&eff_ty) {
                    // Non-scalar return type (Dynamic, List, Object, etc.):
                    // always return a valid *mut FidanValue.  If the operand is
                    // a native scalar (e.g. an Integer local from arithmetic), box it first.
                    lower_operand_boxed(builder, cl_vars, local_types, op, rt, module)?
                } else {
                    // Scalar return type (Integer/Float/Boolean).
                    // If the operand is a Dynamic (boxed pointer) value — e.g. the result
                    // of a dynamic dispatch call that was stored in a Dynamic local — unbox
                    // it to the native scalar the function signature declares.
                    let op_mir_ty = operand_mir_ty(local_types, op);
                    if matches!(op_mir_ty, MirTy::Dynamic) {
                        let raw = lower_operand(builder, cl_vars, op);
                        match &eff_ty {
                            MirTy::Integer => {
                                call_rt(module, builder, rt.unbox_int, &[raw])?.unwrap_or(raw)
                            }
                            MirTy::Float => {
                                call_rt(module, builder, rt.unbox_float, &[raw])?.unwrap_or(raw)
                            }
                            MirTy::Boolean => {
                                call_rt(module, builder, rt.unbox_bool, &[raw])?.unwrap_or(raw)
                            }
                            MirTy::Handle => {
                                call_rt(module, builder, rt.unbox_handle, &[raw])?.unwrap_or(raw)
                            }
                            _ => raw,
                        }
                    } else {
                        // Native scalar source: lower directly and coerce CL type if needed.
                        let v = lower_operand(builder, cl_vars, op);
                        let actual_cl_ty = builder.func.dfg.value_type(v);
                        let expected_cl_ty = mir_ty_to_cl(&eff_ty);
                        if actual_cl_ty != expected_cl_ty {
                            coerce_value(builder, module, rt, v, actual_cl_ty, expected_cl_ty)?
                        } else {
                            v
                        }
                    }
                };
                builder.ins().return_(&[v]);
            } else {
                builder.ins().return_(&[]);
            }
        }
        Terminator::Goto(target) => {
            let args = collect_phi_args(
                module,
                rt,
                builder,
                cl_vars,
                local_types,
                mf,
                bi,
                target.0 as usize,
                _interner,
            )?;
            builder.ins().jump(cl_blocks[target.0 as usize], &args);
        }
        Terminator::Branch {
            cond,
            then_bb,
            else_bb,
        } => {
            let cv = lower_operand(builder, cl_vars, cond);
            // brif requires integer condition; widen if needed.
            let cv64 = widen_to_i64(builder, cv, local_types, cond);
            let then_args = collect_phi_args(
                module,
                rt,
                builder,
                cl_vars,
                local_types,
                mf,
                bi,
                then_bb.0 as usize,
                _interner,
            )?;
            let else_args = collect_phi_args(
                module,
                rt,
                builder,
                cl_vars,
                local_types,
                mf,
                bi,
                else_bb.0 as usize,
                _interner,
            )?;
            builder.ins().brif(
                cv64,
                cl_blocks[then_bb.0 as usize],
                &then_args,
                cl_blocks[else_bb.0 as usize],
                &else_args,
            );
        }
        Terminator::Throw { value } => {
            let v = lower_operand_boxed(builder, cl_vars, local_types, value, rt, module)?;
            // Store the exception in thread-local storage.
            call_rt(module, builder, rt.store_exception, &[v])?;
            if let Some(catch_bid) = current_catch_stack.last() {
                // Direct jump to the catch block — no runtime indirection needed.
                let catch_idx = catch_bid.0 as usize;
                let catch_args = collect_phi_args(
                    module,
                    rt,
                    builder,
                    cl_vars,
                    local_types,
                    mf,
                    bi,
                    catch_idx,
                    _interner,
                )?;
                builder.ins().jump(cl_blocks[catch_idx], &catch_args);
            } else {
                // No local catch handler in this function. Leave the exception in
                // thread-local storage and return early so the caller can either
                // catch it or propagate it further upward.
                emit_exceptional_return(builder, mf);
            }
        }
        Terminator::Unreachable => {
            builder.ins().trap(TrapCode::unwrap_user(4));
        }
    }
    Ok(())
}

// ── Rvalue lowering ────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn lower_rvalue(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    fn_ids: &[cranelift_module::FuncId],
    builder: &mut FunctionBuilder<'_>,
    cl_vars: &[Variable],
    local_types: &HashMap<u32, MirTy>,
    namespace_locals: &HashMap<LocalId, String>,
    program: &MirProgram,
    rval: &Rvalue,
    ty: &MirTy,
    interner: &SymbolInterner,
) -> Result<cranelift_codegen::ir::Value> {
    match rval {
        Rvalue::Use(op) => {
            // When a Dynamic (boxed pointer) value is assigned into a scalar-typed
            // slot (Integer/Float/Boolean) — e.g. after inlining a function that
            // calls dynamic dispatch internally — unbox it to the native scalar.
            let op_mir_ty = operand_mir_ty(local_types, op);
            if matches!(op_mir_ty, MirTy::Dynamic) && is_scalar(ty) {
                let raw = lower_operand(builder, cl_vars, op);
                match ty {
                    MirTy::Integer => {
                        Ok(call_rt(module, builder, rt.unbox_int, &[raw])?.unwrap_or(raw))
                    }
                    MirTy::Float => {
                        Ok(call_rt(module, builder, rt.unbox_float, &[raw])?.unwrap_or(raw))
                    }
                    MirTy::Boolean => {
                        Ok(call_rt(module, builder, rt.unbox_bool, &[raw])?.unwrap_or(raw))
                    }
                    MirTy::Handle => {
                        Ok(call_rt(module, builder, rt.unbox_handle, &[raw])?.unwrap_or(raw))
                    }
                    _ => Ok(raw),
                }
            } else if is_scalar(&op_mir_ty) {
                if is_scalar(ty) {
                    Ok(lower_operand(builder, cl_vars, op))
                } else {
                    lower_operand_boxed(builder, cl_vars, local_types, op, rt, module)
                }
            } else {
                let boxed = lower_operand(builder, cl_vars, op);
                Ok(call_rt(module, builder, rt.clone_any, &[boxed])?.unwrap_or(boxed))
            }
        }

        Rvalue::Literal(lit) => lower_lit(module, builder, rt, lit, interner),

        Rvalue::Binary { op, lhs, rhs } => lower_binary(
            module,
            rt,
            builder,
            cl_vars,
            local_types,
            *op,
            lhs,
            rhs,
            ty,
            interner,
        ),

        Rvalue::Unary { op, operand } => lower_unary(
            module,
            rt,
            builder,
            cl_vars,
            local_types,
            *op,
            operand,
            ty,
            interner,
        ),

        Rvalue::NullCoalesce { lhs, rhs } => {
            let l = lower_operand_boxed(builder, cl_vars, local_types, lhs, rt, module)?;
            let r = lower_operand_boxed(builder, cl_vars, local_types, rhs, rt, module)?;
            Ok(call_rt(module, builder, rt.null_coalesce, &[l, r])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)))
        }

        Rvalue::Call { callee, args } => {
            let ret = emit_call(
                module,
                rt,
                fn_ids,
                builder,
                cl_vars,
                local_types,
                namespace_locals,
                program,
                callee,
                args,
                ty,
                interner,
            )?;
            let ret = ret.unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
            let expected_cl_ty = mir_ty_to_cl(ty);
            let actual_cl_ty = builder.func.dfg.value_type(ret);
            if actual_cl_ty != expected_cl_ty {
                let coerced = coerce_value(builder, module, rt, ret, actual_cl_ty, expected_cl_ty)?;
                if actual_cl_ty == PTR_TY && is_scalar(ty) {
                    call_rt(module, builder, rt.drop_any, &[ret])?;
                }
                Ok(coerced)
            } else {
                Ok(ret)
            }
        }

        Rvalue::Construct {
            ty: class_sym,
            fields,
        } => {
            let class_name = interner.resolve(*class_sym);
            let (cp, cl) = str_const(module, builder, class_name.as_ref())?;
            let obj = call_rt(module, builder, rt.obj_new, &[cp, cl])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
            for (fsym, fop) in fields {
                let fv = lower_operand_boxed(builder, cl_vars, local_types, fop, rt, module)?;
                let (fp, fl) = str_const(module, builder, interner.resolve(*fsym).as_ref())?;
                call_rt(module, builder, rt.obj_set_field, &[obj, fp, fl, fv])?;
            }
            // Store each method (from the class and all ancestors) as a callable
            // FidanValue::Function in the dict, keyed by "__method__<name>".
            // Child methods take precedence over parent methods (via `entry().or_insert`).
            {
                use fidan_lexer::Symbol;
                let mut method_map: HashMap<Symbol, MirFunctionId> = HashMap::new();
                let mut curr_sym = Some(*class_sym);
                while let Some(sym) = curr_sym {
                    if let Some(obj_info) = program.objects.iter().find(|o| o.name == sym) {
                        for (&msym, &fid) in &obj_info.methods {
                            method_map.entry(msym).or_insert(fid);
                        }
                        curr_sym = obj_info.parent;
                    } else {
                        break;
                    }
                }
                for (msym, fid) in &method_map {
                    let mname = interner.resolve(*msym);
                    let key = format!("__method__{}", mname.as_ref());
                    let (kp, kl) = str_const(module, builder, &key)?;
                    let fn_id_val = builder.ins().iconst(I64, fid.0 as i64);
                    let fn_ref = call_rt(module, builder, rt.box_fn_ref, &[fn_id_val])?
                        .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
                    call_rt(module, builder, rt.obj_set_field, &[obj, kp, kl, fn_ref])?;
                }
            }
            Ok(obj)
        }

        Rvalue::List(elems) => {
            let list = call_rt(module, builder, rt.list_new, &[])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
            for elem in elems {
                let ev = lower_operand_boxed(builder, cl_vars, local_types, elem, rt, module)?;
                call_rt(module, builder, rt.list_push, &[list, ev])?;
            }
            Ok(list)
        }

        Rvalue::Dict(pairs) => {
            let dict = call_rt(module, builder, rt.dict_new, &[])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
            for (k, v) in pairs {
                let kv = lower_operand_boxed(builder, cl_vars, local_types, k, rt, module)?;
                let vv = lower_operand_boxed(builder, cl_vars, local_types, v, rt, module)?;
                call_rt(module, builder, rt.dict_set, &[dict, kv, vv])?;
            }
            Ok(dict)
        }

        Rvalue::Tuple(elems) => {
            let (items_ptr, item_count) =
                build_ptr_array(module, rt, builder, cl_vars, local_types, elems, interner)?;
            Ok(
                call_rt(module, builder, rt.tuple_pack, &[items_ptr, item_count])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)),
            )
        }

        Rvalue::StringInterp(parts) => {
            lower_string_interp(module, rt, builder, cl_vars, local_types, parts, interner)
        }

        Rvalue::CatchException => Ok(call_rt(module, builder, rt.catch_exception, &[])?
            .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0))),

        Rvalue::MakeClosure { fn_id, captures } => {
            let id = builder.ins().iconst(I64, *fn_id as i64);
            if captures.is_empty() {
                // No captures — a plain function box is sufficient.
                Ok(call_rt(module, builder, rt.box_fn_ref, &[id])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)))
            } else {
                // Build a stack array of boxed capture values, then call
                // fdn_make_closure(fn_id, captures_ptr, captures_cnt).
                let (arr, cnt) = build_ptr_array(
                    module,
                    rt,
                    builder,
                    cl_vars,
                    local_types,
                    captures,
                    interner,
                )?;
                Ok(call_rt(module, builder, rt.make_closure, &[id, arr, cnt])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)))
            }
        }

        Rvalue::Slice {
            target,
            start,
            end,
            inclusive,
            step,
        } => {
            let obj = lower_operand_as_ptr(builder, cl_vars, local_types, target, rt, module)?;
            let start_v = match start {
                Some(o) => lower_operand_boxed(builder, cl_vars, local_types, o, rt, module)?,
                None => call_rt(module, builder, rt.box_nothing, &[])?.unwrap(),
            };
            let end_v = match end {
                Some(o) => lower_operand_boxed(builder, cl_vars, local_types, o, rt, module)?,
                None => call_rt(module, builder, rt.box_nothing, &[])?.unwrap(),
            };
            let step_v = match step {
                Some(o) => lower_operand_boxed(builder, cl_vars, local_types, o, rt, module)?,
                None => call_rt(module, builder, rt.box_nothing, &[])?.unwrap(),
            };
            let inc_v = builder.ins().iconst(I8, if *inclusive { 1 } else { 0 });
            Ok(call_rt(
                module,
                builder,
                rt.slice_fn,
                &[obj, start_v, end_v, inc_v, step_v],
            )?
            .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)))
        }

        Rvalue::ConstructEnum { tag, payload } => {
            let (tp, tl) = str_const(module, builder, interner.resolve(*tag).as_ref())?;
            let (arr, cnt) =
                build_ptr_array(module, rt, builder, cl_vars, local_types, payload, interner)?;
            Ok(
                call_rt(module, builder, rt.enum_variant, &[tp, tl, arr, cnt])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)),
            )
        }

        Rvalue::EnumTagCheck {
            value,
            expected_tag,
        } => {
            let vp = lower_operand_as_ptr(builder, cl_vars, local_types, value, rt, module)?;
            let (tp, tl) = str_const(module, builder, interner.resolve(*expected_tag).as_ref())?;
            let i8result = call_rt(module, builder, rt.enum_tag_check, &[vp, tp, tl])?
                .unwrap_or_else(|| builder.ins().iconst(I8, 0));
            Ok(i8result)
        }

        Rvalue::EnumPayload { value, index } => {
            let vp = lower_operand_as_ptr(builder, cl_vars, local_types, value, rt, module)?;
            let idx = builder.ins().iconst(I64, *index as i64);
            Ok(call_rt(module, builder, rt.enum_payload, &[vp, idx])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)))
        }
    }
}

// ── Literal lowering ───────────────────────────────────────────────────────────

fn lower_lit(
    module: &mut ObjectModule,
    builder: &mut FunctionBuilder<'_>,
    rt: &RuntimeDecls,
    lit: &MirLit,
    _interner: &SymbolInterner,
) -> Result<cranelift_codegen::ir::Value> {
    match lit {
        MirLit::Int(n) => Ok(builder.ins().iconst(I64, *n)),
        MirLit::Float(f) => Ok(builder.ins().f64const(*f)),
        MirLit::Bool(b) => Ok(builder.ins().iconst(I8, *b as i64)),
        MirLit::Nothing => Ok(call_rt(module, builder, rt.box_nothing, &[])?
            .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0))),
        MirLit::Str(s) => {
            let (p, l) = str_const(module, builder, s)?;
            Ok(call_rt(module, builder, rt.box_str, &[p, l])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)))
        }
        MirLit::FunctionRef(id) => {
            let v = builder.ins().iconst(I64, *id as i64);
            Ok(call_rt(module, builder, rt.box_fn_ref, &[v])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)))
        }
        MirLit::Namespace(m) => {
            let (p, l) = str_const(module, builder, m)?;
            Ok(call_rt(module, builder, rt.box_namespace, &[p, l])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)))
        }
        MirLit::EnumType(m) => {
            let (p, l) = str_const(module, builder, m)?;
            Ok(call_rt(module, builder, rt.box_enum_type, &[p, l])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)))
        }
        MirLit::ClassType(m) => {
            let (p, l) = str_const(module, builder, m)?;
            Ok(call_rt(module, builder, rt.box_class_type, &[p, l])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)))
        }
        MirLit::StdlibFn { module: m, name } => {
            let (mp, ml) = str_const(module, builder, m)?;
            let (np, nl) = str_const(module, builder, name)?;
            Ok(
                call_rt(module, builder, rt.box_stdlib_fn, &[mp, ml, np, nl])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)),
            )
        }
    }
}

// ── Binary / Unary operations ──────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn lower_binary(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    builder: &mut FunctionBuilder<'_>,
    cl_vars: &[Variable],
    local_types: &HashMap<u32, MirTy>,
    op: fidan_ast::BinOp,
    lhs: &Operand,
    rhs: &Operand,
    result_ty: &MirTy,
    _interner: &SymbolInterner,
) -> Result<cranelift_codegen::ir::Value> {
    use fidan_ast::BinOp::*;

    fn known_nonzero_integer(op: &Operand) -> bool {
        matches!(op, Operand::Const(MirLit::Int(value)) if *value != 0)
    }

    let lhs_mir = operand_mir_ty(local_types, lhs);
    let rhs_mir = operand_mir_ty(local_types, rhs);
    let lhs_ty = mir_ty_to_cl(&lhs_mir);
    let rhs_ty = mir_ty_to_cl(&rhs_mir);

    // Integer × Integer native path — only when both operands are DEFINITELY
    // native integers (not Dynamic/boxed pointers, which also map to I64).
    if lhs_ty == I64 && rhs_ty == I64 && lhs_mir == MirTy::Integer && rhs_mir == MirTy::Integer {
        let l = lower_operand(builder, cl_vars, lhs);
        let r = lower_operand(builder, cl_vars, rhs);
        return Ok(match op {
            Add => builder.ins().iadd(l, r),
            Sub => builder.ins().isub(l, r),
            Mul => builder.ins().imul(l, r),
            Div => {
                if !known_nonzero_integer(rhs) {
                    // Guard against divide-by-zero only when the divisor is not a known nonzero literal.
                    let zero = builder.ins().iconst(I64, 0);
                    let is_zero = builder.ins().icmp(IntCC::Equal, r, zero);
                    let ok_block = builder.create_block();
                    let trap_block = builder.create_block();
                    builder.ins().brif(is_zero, trap_block, &[], ok_block, &[]);
                    builder.switch_to_block(trap_block);
                    builder.seal_block(trap_block);
                    let (mp, ml) = str_const(module, builder, "division by zero")?;
                    let msg_ptr = {
                        let r2 = module.declare_func_in_func(rt.box_str, builder.func);
                        let inst = builder.ins().call(r2, &[mp, ml]);
                        builder.inst_results(inst)[0]
                    };
                    let panic_ref = module.declare_func_in_func(rt.panic_fn, builder.func);
                    builder.ins().call(panic_ref, &[msg_ptr]);
                    builder.ins().trap(TrapCode::unwrap_user(1));
                    builder.switch_to_block(ok_block);
                    builder.seal_block(ok_block);
                }
                builder.ins().sdiv(l, r)
            }
            Rem => {
                if !known_nonzero_integer(rhs) {
                    // Guard against divide-by-zero only when the divisor is not a known nonzero literal.
                    let zero = builder.ins().iconst(I64, 0);
                    let is_zero = builder.ins().icmp(IntCC::Equal, r, zero);
                    let ok_block = builder.create_block();
                    let trap_block = builder.create_block();
                    builder.ins().brif(is_zero, trap_block, &[], ok_block, &[]);
                    builder.switch_to_block(trap_block);
                    builder.seal_block(trap_block);
                    let (mp, ml) = str_const(module, builder, "remainder by zero")?;
                    let msg_ptr = {
                        let r2 = module.declare_func_in_func(rt.box_str, builder.func);
                        let inst = builder.ins().call(r2, &[mp, ml]);
                        builder.inst_results(inst)[0]
                    };
                    let panic_ref = module.declare_func_in_func(rt.panic_fn, builder.func);
                    builder.ins().call(panic_ref, &[msg_ptr]);
                    builder.ins().trap(TrapCode::unwrap_user(1));
                    builder.switch_to_block(ok_block);
                    builder.seal_block(ok_block);
                }
                builder.ins().srem(l, r)
            }
            Eq => builder.ins().icmp(IntCC::Equal, l, r),
            NotEq => builder.ins().icmp(IntCC::NotEqual, l, r),
            Lt => builder.ins().icmp(IntCC::SignedLessThan, l, r),
            LtEq => builder.ins().icmp(IntCC::SignedLessThanOrEqual, l, r),
            Gt => builder.ins().icmp(IntCC::SignedGreaterThan, l, r),
            GtEq => builder.ins().icmp(IntCC::SignedGreaterThanOrEqual, l, r),
            BitXor => builder.ins().bxor(l, r),
            BitAnd => builder.ins().band(l, r),
            BitOr => builder.ins().bor(l, r),
            Shl => builder.ins().ishl(l, r),
            Shr => builder.ins().sshr(l, r),
            Range | RangeInclusive => {
                let inc = builder
                    .ins()
                    .iconst(I8, if op == RangeInclusive { 1 } else { 0 });
                return Ok(call_rt(module, builder, rt.make_range, &[l, r, inc])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)));
            }
            Pow | And | Or => {
                // Box and dispatch through C ABI.
                let lb = call_rt(module, builder, rt.box_int, &[l])?.unwrap();
                let rb = call_rt(module, builder, rt.box_int, &[r])?.unwrap();
                return dyn_binop(module, rt, builder, op, lb, rb, result_ty);
            }
        });
    }

    // Float × Float native path
    if lhs_ty == F64 && rhs_ty == F64 && lhs_mir == MirTy::Float && rhs_mir == MirTy::Float {
        let l = lower_operand(builder, cl_vars, lhs);
        let r = lower_operand(builder, cl_vars, rhs);
        return Ok(match op {
            Add => builder.ins().fadd(l, r),
            Sub => builder.ins().fsub(l, r),
            Mul => builder.ins().fmul(l, r),
            Div => builder.ins().fdiv(l, r),
            Rem => {
                let lb = call_rt(module, builder, rt.box_float, &[l])?.unwrap();
                let rb = call_rt(module, builder, rt.box_float, &[r])?.unwrap();
                return dyn_binop(module, rt, builder, op, lb, rb, result_ty);
            }
            Eq => builder.ins().fcmp(FloatCC::Equal, l, r),
            NotEq => builder.ins().fcmp(FloatCC::NotEqual, l, r),
            Lt => builder.ins().fcmp(FloatCC::LessThan, l, r),
            LtEq => builder.ins().fcmp(FloatCC::LessThanOrEqual, l, r),
            Gt => builder.ins().fcmp(FloatCC::GreaterThan, l, r),
            GtEq => builder.ins().fcmp(FloatCC::GreaterThanOrEqual, l, r),
            _ => {
                let lb = call_rt(module, builder, rt.box_float, &[l])?.unwrap();
                let rb = call_rt(module, builder, rt.box_float, &[r])?.unwrap();
                return dyn_binop(module, rt, builder, op, lb, rb, result_ty);
            }
        });
    }

    // Boolean × Boolean
    if lhs_ty == I8 && rhs_ty == I8 {
        let l = lower_operand(builder, cl_vars, lhs);
        let r = lower_operand(builder, cl_vars, rhs);
        match op {
            And => return Ok(builder.ins().band(l, r)),
            Or => return Ok(builder.ins().bor(l, r)),
            Eq => return Ok(builder.ins().icmp(IntCC::Equal, l, r)),
            NotEq => return Ok(builder.ins().icmp(IntCC::NotEqual, l, r)),
            _ => {}
        }
    }

    // Fallback: box both and dispatch dynamically.
    let lb = lower_operand_boxed(builder, cl_vars, local_types, lhs, rt, module)?;
    let rb = lower_operand_boxed(builder, cl_vars, local_types, rhs, rt, module)?;
    dyn_binop(module, rt, builder, op, lb, rb, result_ty)
}

fn dyn_binop(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    builder: &mut FunctionBuilder<'_>,
    op: fidan_ast::BinOp,
    l: cranelift_codegen::ir::Value,
    r: cranelift_codegen::ir::Value,
    result_ty: &MirTy,
) -> Result<cranelift_codegen::ir::Value> {
    use fidan_ast::BinOp::*;
    let rt_fn = match op {
        Add => rt.dyn_add,
        Sub => rt.dyn_sub,
        Mul => rt.dyn_mul,
        Div => rt.dyn_div,
        Rem => rt.dyn_rem,
        Pow => rt.dyn_pow,
        Eq => rt.dyn_eq,
        NotEq => rt.dyn_ne,
        Lt => rt.dyn_lt,
        LtEq => rt.dyn_le,
        Gt => rt.dyn_gt,
        GtEq => rt.dyn_ge,
        And => rt.dyn_and,
        Or => rt.dyn_or,
        BitXor => rt.dyn_bit_xor,
        BitAnd => rt.dyn_bit_and,
        BitOr => rt.dyn_bit_or,
        Shl => rt.dyn_shl,
        Shr => rt.dyn_shr,
        Range | RangeInclusive => {
            let start = call_rt(module, builder, rt.unbox_int, &[l])?.unwrap();
            let end = call_rt(module, builder, rt.unbox_int, &[r])?.unwrap();
            let inc = builder
                .ins()
                .iconst(I8, if op == RangeInclusive { 1 } else { 0 });
            return Ok(call_rt(module, builder, rt.make_range, &[start, end, inc])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)));
        }
    };
    let boxed = call_rt(module, builder, rt_fn, &[l, r])?
        .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
    // Comparison operators (fdn_dyn_eq etc.) return a native i8 bool directly.
    // All other operators return a boxed *mut FidanValue (PTR_TY = i64).
    // We must NOT call unbox_bool on an i8 returned by a comparison.
    let is_comparison = matches!(op, Eq | NotEq | Lt | LtEq | Gt | GtEq);
    if is_comparison {
        // `boxed` is already an i8 native bool
        match result_ty {
            MirTy::Boolean => Ok(boxed), // already i8 — perfect match
            _ => {
                // Caller wants a boxed pointer (Dynamic/Integer/etc.): box the bool
                let r = module.declare_func_in_func(rt.box_bool, builder.func);
                let inst = builder.ins().call(r, &[boxed]);
                Ok(builder.inst_results(inst)[0])
            }
        }
    } else {
        // `boxed` is i64 (a *mut FidanValue heap pointer)
        match result_ty {
            MirTy::Integer => {
                Ok(call_rt(module, builder, rt.unbox_int, &[boxed])?.unwrap_or(boxed))
            }
            MirTy::Float => {
                Ok(call_rt(module, builder, rt.unbox_float, &[boxed])?.unwrap_or(boxed))
            }
            MirTy::Boolean => {
                Ok(call_rt(module, builder, rt.unbox_bool, &[boxed])?.unwrap_or(boxed))
            }
            MirTy::Handle => {
                Ok(call_rt(module, builder, rt.unbox_handle, &[boxed])?.unwrap_or(boxed))
            }
            _ => Ok(boxed), // Dynamic / String / Range: keep the boxed ptr
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_unary(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    builder: &mut FunctionBuilder<'_>,
    cl_vars: &[Variable],
    local_types: &HashMap<u32, MirTy>,
    op: fidan_ast::UnOp,
    operand: &Operand,
    ty: &MirTy,
    _interner: &SymbolInterner,
) -> Result<cranelift_codegen::ir::Value> {
    use fidan_ast::UnOp::*;
    let oty = operand_ty(cl_vars, local_types, operand);
    match (op, oty) {
        (Neg, I64) => {
            let v = lower_operand(builder, cl_vars, operand);
            Ok(builder.ins().ineg(v))
        }
        (Neg, F64) => {
            let v = lower_operand(builder, cl_vars, operand);
            Ok(builder.ins().fneg(v))
        }
        (Not, I8) => {
            let v = lower_operand(builder, cl_vars, operand);
            Ok(builder.ins().bnot(v))
        }
        (Pos, _) => Ok(lower_operand(builder, cl_vars, operand)),
        _ => {
            let v = lower_operand_boxed(builder, cl_vars, local_types, operand, rt, module)?;
            let rt_fn = match op {
                Neg => rt.dyn_neg,
                Not => rt.dyn_not,
                Pos => rt.clone_any,
            };
            let boxed = call_rt(module, builder, rt_fn, &[v])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
            // Unbox if the caller expects a native scalar
            match ty {
                MirTy::Integer => {
                    Ok(call_rt(module, builder, rt.unbox_int, &[boxed])?.unwrap_or(boxed))
                }
                MirTy::Float => {
                    Ok(call_rt(module, builder, rt.unbox_float, &[boxed])?.unwrap_or(boxed))
                }
                MirTy::Boolean => {
                    Ok(call_rt(module, builder, rt.unbox_bool, &[boxed])?.unwrap_or(boxed))
                }
                _ => Ok(boxed),
            }
        }
    }
}

// ── Call emission ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn emit_call(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    fn_ids: &[cranelift_module::FuncId],
    builder: &mut FunctionBuilder<'_>,
    cl_vars: &[Variable],
    local_types: &HashMap<u32, MirTy>,
    namespace_locals: &HashMap<LocalId, String>,
    program: &MirProgram,
    callee: &Callee,
    args: &[Operand],
    result_ty: &MirTy,
    interner: &SymbolInterner,
) -> Result<Option<cranelift_codegen::ir::Value>> {
    match callee {
        Callee::Fn(fn_id) => {
            let cl_fn_id = fn_ids[fn_id.0 as usize];
            let fn_ref = module.declare_func_in_func(cl_fn_id, builder.func);
            let mir_fn = &program.functions[fn_id.0 as usize];
            let mut arg_vals = Vec::with_capacity(mir_fn.params.len());
            for (i, param) in mir_fn.params.iter().enumerate() {
                let v = if let Some(arg_op) = args.get(i) {
                    lower_operand_coerced(
                        builder,
                        cl_vars,
                        local_types,
                        arg_op,
                        &param.ty,
                        rt,
                        module,
                    )?
                } else if let Some(default_lit) = &param.default {
                    let raw = lower_lit(module, builder, rt, default_lit, interner)?;
                    let expected_cl_ty = mir_ty_to_cl(&param.ty);
                    let actual_cl_ty = builder.func.dfg.value_type(raw);
                    if actual_cl_ty != expected_cl_ty {
                        coerce_value(builder, module, rt, raw, actual_cl_ty, expected_cl_ty)?
                    } else {
                        raw
                    }
                } else {
                    bail!(
                        "internal compiler bug: missing required argument {} when lowering call to `{}`",
                        i,
                        interner.resolve(mir_fn.name)
                    );
                };
                arg_vals.push(v);
            }
            let call = builder.ins().call(fn_ref, &arg_vals);
            let results = builder.inst_results(call);
            Ok(results.first().copied())
        }

        Callee::Builtin(sym) => {
            let name = interner.resolve(*sym);
            emit_builtin(
                module,
                rt,
                builder,
                cl_vars,
                local_types,
                name.as_ref(),
                args,
                result_ty,
                interner,
            )
            .map(Some)
        }

        Callee::Method { receiver, method } => {
            let method_name = interner.resolve(*method);
            if let Some(ns) = stdlib_namespace(receiver, namespace_locals)
                && let Some(val) = emit_stdlib_method_call(
                    module,
                    rt,
                    builder,
                    cl_vars,
                    local_types,
                    ns.as_str(),
                    method_name.as_ref(),
                    args,
                    result_ty,
                )?
            {
                return Ok(Some(val));
            }

            if let Some(val) = emit_container_method_call(
                module,
                rt,
                builder,
                cl_vars,
                local_types,
                receiver,
                method_name.as_ref(),
                args,
                result_ty,
            )? {
                return Ok(Some(val));
            }

            let recv = lower_operand_as_ptr(builder, cl_vars, local_types, receiver, rt, module)?;
            let (mp, ml) = str_const(module, builder, method_name.as_ref())?;
            let (arr, cnt) =
                build_ptr_array(module, rt, builder, cl_vars, local_types, args, interner)?;
            let boxed = call_rt(module, builder, rt.obj_invoke, &[recv, mp, ml, arr, cnt])?;
            if let Some(boxed) = boxed {
                let coerced = coerce_boxed_call_result(module, rt, builder, boxed, result_ty)?;
                Ok(Some(coerced))
            } else {
                Ok(None)
            }
        }

        Callee::Dynamic(fn_op) => {
            // fdn_call_dynamic(fn_val: *mut FidanValue, args_ptr, args_cnt) -> *mut FidanValue
            let fn_val = lower_operand_boxed(builder, cl_vars, local_types, fn_op, rt, module)?;
            let (arr, cnt) =
                build_ptr_array(module, rt, builder, cl_vars, local_types, args, interner)?;
            let boxed = call_rt(module, builder, rt.call_dynamic, &[fn_val, arr, cnt])?;
            if let Some(boxed) = boxed {
                let coerced = coerce_boxed_call_result(module, rt, builder, boxed, result_ty)?;
                Ok(Some(coerced))
            } else {
                Ok(None)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn coerce_boxed_call_result(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    builder: &mut FunctionBuilder<'_>,
    boxed: cranelift_codegen::ir::Value,
    result_ty: &MirTy,
) -> Result<cranelift_codegen::ir::Value> {
    let value = match result_ty {
        MirTy::Integer => call_rt(module, builder, rt.unbox_int, &[boxed])?.unwrap_or(boxed),
        MirTy::Float => call_rt(module, builder, rt.unbox_float, &[boxed])?.unwrap_or(boxed),
        MirTy::Boolean => call_rt(module, builder, rt.unbox_bool, &[boxed])?.unwrap_or(boxed),
        MirTy::Handle => call_rt(module, builder, rt.unbox_handle, &[boxed])?.unwrap_or(boxed),
        _ => boxed,
    };

    if is_scalar(result_ty) {
        call_rt(module, builder, rt.drop_any, &[boxed])?;
    }

    Ok(value)
}

fn box_container_scalar_result(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    builder: &mut FunctionBuilder<'_>,
    value: cranelift_codegen::ir::Value,
    scalar_ty: &MirTy,
    result_ty: &MirTy,
) -> Result<cranelift_codegen::ir::Value> {
    let boxed = match scalar_ty {
        MirTy::Integer => call_rt(module, builder, rt.box_int, &[value])?.unwrap_or(value),
        MirTy::Float => call_rt(module, builder, rt.box_float, &[value])?.unwrap_or(value),
        MirTy::Boolean => call_rt(module, builder, rt.box_bool, &[value])?.unwrap_or(value),
        MirTy::Handle => call_rt(module, builder, rt.box_handle, &[value])?.unwrap_or(value),
        _ => value,
    };
    coerce_boxed_call_result(module, rt, builder, boxed, result_ty)
}

#[allow(clippy::too_many_arguments)]
fn emit_container_method_call(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    builder: &mut FunctionBuilder<'_>,
    cl_vars: &[Variable],
    local_types: &HashMap<u32, MirTy>,
    receiver: &Operand,
    method_name: &str,
    args: &[Operand],
    result_ty: &MirTy,
) -> Result<Option<cranelift_codegen::ir::Value>> {
    let receiver_kind = match operand_mir_ty(local_types, receiver) {
        MirTy::Dict(_, _) => ReceiverBuiltinKind::Dict,
        MirTy::HashSet(_) => ReceiverBuiltinKind::HashSet,
        _ => return Ok(None),
    };
    let Some(operation) =
        infer_receiver_member(receiver_kind, method_name).and_then(|info| info.operation)
    else {
        return Ok(None);
    };

    let recv = lower_operand_as_ptr(builder, cl_vars, local_types, receiver, rt, module)?;
    let boxed = match (receiver_kind, operation) {
        (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Len) => {
            let len = call_rt(module, builder, rt.dict_len, &[recv])?.unwrap_or(recv);
            return box_container_scalar_result(
                module,
                rt,
                builder,
                len,
                &MirTy::Integer,
                result_ty,
            )
            .map(Some);
        }
        (ReceiverBuiltinKind::Dict, ReceiverMethodOp::IsEmpty) => {
            let len = call_rt(module, builder, rt.dict_len, &[recv])?.unwrap_or(recv);
            let is_empty = builder.ins().icmp_imm(IntCC::Equal, len, 0);
            let is_empty = builder.ins().uextend(I8, is_empty);
            return box_container_scalar_result(
                module,
                rt,
                builder,
                is_empty,
                &MirTy::Boolean,
                result_ty,
            )
            .map(Some);
        }
        (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Get) => {
            let Some(key) = args.first() else {
                return Ok(Some(
                    call_rt(module, builder, rt.box_nothing, &[])?.unwrap(),
                ));
            };
            let key = lower_operand_boxed(builder, cl_vars, local_types, key, rt, module)?;
            call_rt(module, builder, rt.dict_get, &[recv, key])?.unwrap_or(recv)
        }
        (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Set) => {
            if let (Some(key), Some(value)) = (args.first(), args.get(1)) {
                let key = lower_operand_boxed(builder, cl_vars, local_types, key, rt, module)?;
                let value = lower_operand_boxed(builder, cl_vars, local_types, value, rt, module)?;
                call_rt(module, builder, rt.dict_set, &[recv, key, value])?;
            }
            call_rt(module, builder, rt.box_nothing, &[])?.unwrap_or(recv)
        }
        (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Contains) => {
            let Some(key) = args.first() else {
                let zero = builder.ins().iconst(I8, 0);
                return box_container_scalar_result(
                    module,
                    rt,
                    builder,
                    zero,
                    &MirTy::Boolean,
                    result_ty,
                )
                .map(Some);
            };
            let key = lower_operand_boxed(builder, cl_vars, local_types, key, rt, module)?;
            let contains = call_rt(module, builder, rt.dict_contains_key, &[recv, key])?
                .unwrap_or_else(|| builder.ins().iconst(I8, 0));
            return box_container_scalar_result(
                module,
                rt,
                builder,
                contains,
                &MirTy::Boolean,
                result_ty,
            )
            .map(Some);
        }
        (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Remove) => {
            if let Some(key) = args.first() {
                let key = lower_operand_boxed(builder, cl_vars, local_types, key, rt, module)?;
                call_rt(module, builder, rt.dict_remove, &[recv, key])?;
            }
            call_rt(module, builder, rt.box_nothing, &[])?.unwrap_or(recv)
        }
        (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Keys) => {
            call_rt(module, builder, rt.dict_keys, &[recv])?.unwrap_or(recv)
        }
        (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Values) => {
            call_rt(module, builder, rt.dict_values, &[recv])?.unwrap_or(recv)
        }
        (ReceiverBuiltinKind::Dict, ReceiverMethodOp::Entries) => {
            call_rt(module, builder, rt.dict_entries, &[recv])?.unwrap_or(recv)
        }
        (ReceiverBuiltinKind::Dict, ReceiverMethodOp::ToString) => {
            call_rt(module, builder, rt.to_string, &[recv])?.unwrap_or(recv)
        }
        (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::Len) => {
            let len = call_rt(module, builder, rt.len_fn, &[recv])?.unwrap_or(recv);
            return box_container_scalar_result(
                module,
                rt,
                builder,
                len,
                &MirTy::Integer,
                result_ty,
            )
            .map(Some);
        }
        (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::IsEmpty) => {
            let len = call_rt(module, builder, rt.len_fn, &[recv])?.unwrap_or(recv);
            let is_empty = builder.ins().icmp_imm(IntCC::Equal, len, 0);
            let is_empty = builder.ins().uextend(I8, is_empty);
            return box_container_scalar_result(
                module,
                rt,
                builder,
                is_empty,
                &MirTy::Boolean,
                result_ty,
            )
            .map(Some);
        }
        (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::Insert) => {
            if let Some(value) = args.first() {
                let value = lower_operand_boxed(builder, cl_vars, local_types, value, rt, module)?;
                call_rt(module, builder, rt.hashset_insert, &[recv, value])?;
            }
            call_rt(module, builder, rt.box_nothing, &[])?.unwrap_or(recv)
        }
        (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::Remove) => {
            if let Some(value) = args.first() {
                let value = lower_operand_boxed(builder, cl_vars, local_types, value, rt, module)?;
                call_rt(module, builder, rt.hashset_remove, &[recv, value])?;
            }
            call_rt(module, builder, rt.box_nothing, &[])?.unwrap_or(recv)
        }
        (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::Contains) => {
            let Some(value) = args.first() else {
                let zero = builder.ins().iconst(I8, 0);
                return box_container_scalar_result(
                    module,
                    rt,
                    builder,
                    zero,
                    &MirTy::Boolean,
                    result_ty,
                )
                .map(Some);
            };
            let value = lower_operand_boxed(builder, cl_vars, local_types, value, rt, module)?;
            let contains = call_rt(module, builder, rt.hashset_contains, &[recv, value])?
                .unwrap_or_else(|| builder.ins().iconst(I8, 0));
            return box_container_scalar_result(
                module,
                rt,
                builder,
                contains,
                &MirTy::Boolean,
                result_ty,
            )
            .map(Some);
        }
        (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::ToList) => {
            call_rt(module, builder, rt.hashset_to_list, &[recv])?.unwrap_or(recv)
        }
        (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::Union) => {
            let Some(other) = args.first() else {
                return Ok(Some(
                    call_rt(module, builder, rt.box_nothing, &[])?.unwrap(),
                ));
            };
            let other = lower_operand_boxed(builder, cl_vars, local_types, other, rt, module)?;
            call_rt(module, builder, rt.hashset_union, &[recv, other])?.unwrap_or(recv)
        }
        (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::Intersect) => {
            let Some(other) = args.first() else {
                return Ok(Some(
                    call_rt(module, builder, rt.box_nothing, &[])?.unwrap(),
                ));
            };
            let other = lower_operand_boxed(builder, cl_vars, local_types, other, rt, module)?;
            call_rt(module, builder, rt.hashset_intersect, &[recv, other])?.unwrap_or(recv)
        }
        (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::Diff) => {
            let Some(other) = args.first() else {
                return Ok(Some(
                    call_rt(module, builder, rt.box_nothing, &[])?.unwrap(),
                ));
            };
            let other = lower_operand_boxed(builder, cl_vars, local_types, other, rt, module)?;
            call_rt(module, builder, rt.hashset_diff, &[recv, other])?.unwrap_or(recv)
        }
        (ReceiverBuiltinKind::HashSet, ReceiverMethodOp::ToString) => {
            call_rt(module, builder, rt.to_string, &[recv])?.unwrap_or(recv)
        }
        _ => return Ok(None),
    };

    coerce_boxed_call_result(module, rt, builder, boxed, result_ty).map(Some)
}

#[allow(clippy::too_many_arguments)]
fn emit_stdlib_method_call(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    builder: &mut FunctionBuilder<'_>,
    vars: &[Variable],
    _local_types: &HashMap<u32, MirTy>,
    ns: &str,
    method: &str,
    args: &[Operand],
    result_ty: &MirTy,
) -> Result<Option<cranelift_codegen::ir::Value>> {
    let Some(first_arg) = args.first() else {
        return Ok(None);
    };

    let arg_kinds = [operand_stdlib_kind(first_arg, _local_types)];
    let Some(info) = infer_stdlib_method(ns, method, &arg_kinds) else {
        return Ok(None);
    };

    let Some(intrinsic) = info.intrinsic else {
        return Ok(None);
    };

    let arg = lower_operand(builder, vars, first_arg);
    let arg_ty = builder.func.dfg.value_type(arg);
    let fval = if arg_ty == F64 {
        arg
    } else {
        builder.ins().fcvt_from_sint(F64, arg)
    };

    let result = match intrinsic {
        StdlibIntrinsic::Math(MathIntrinsic::Sqrt) => {
            StdlibResultValue::Float(builder.ins().sqrt(fval))
        }
        StdlibIntrinsic::Math(MathIntrinsic::Abs) => {
            if arg_ty == F64 {
                StdlibResultValue::Float(builder.ins().fabs(arg))
            } else {
                StdlibResultValue::Integer(builder.ins().iabs(arg))
            }
        }
        StdlibIntrinsic::Math(MathIntrinsic::Floor) => {
            let floored = builder.ins().floor(fval);
            StdlibResultValue::Integer(builder.ins().fcvt_to_sint(I64, floored))
        }
        StdlibIntrinsic::Math(MathIntrinsic::Ceil) => {
            let ceiled = builder.ins().ceil(fval);
            StdlibResultValue::Integer(builder.ins().fcvt_to_sint(I64, ceiled))
        }
        StdlibIntrinsic::Math(MathIntrinsic::Trunc) => {
            StdlibResultValue::Float(builder.ins().trunc(fval))
        }
    };

    match result {
        StdlibResultValue::Integer(value) => {
            if matches!(result_ty, MirTy::Dynamic) {
                Ok(call_rt(module, builder, rt.box_int, &[value])?)
            } else {
                Ok(Some(value))
            }
        }
        StdlibResultValue::Float(value) => {
            if matches!(result_ty, MirTy::Dynamic) {
                Ok(call_rt(module, builder, rt.box_float, &[value])?)
            } else {
                Ok(Some(value))
            }
        }
    }
}

fn build_global_namespace_map(
    program: &MirProgram,
    interner: &SymbolInterner,
) -> HashMap<GlobalId, String> {
    let mut global_ns_map = HashMap::new();
    for (i, global) in program.globals.iter().enumerate() {
        let global_name = interner.resolve(global.name);
        for decl in &program.use_decls {
            if decl.is_stdlib
                && decl.specific_names.is_none()
                && global_name.as_ref() == decl.alias.as_str()
            {
                global_ns_map.insert(GlobalId(i as u32), decl.module.clone());
            }
        }
    }
    global_ns_map
}

// ── Builtin dispatch ───────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn emit_builtin(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    builder: &mut FunctionBuilder<'_>,
    cl_vars: &[Variable],
    local_types: &HashMap<u32, MirTy>,
    name: &str,
    args: &[Operand],
    result_ty: &MirTy,
    interner: &SymbolInterner,
) -> Result<cranelift_codegen::ir::Value> {
    match name {
        "print" => {
            if args.len() <= 1 {
                let arg = if args.is_empty() {
                    call_rt(module, builder, rt.box_nothing, &[])?.unwrap()
                } else {
                    lower_operand_boxed(builder, cl_vars, local_types, &args[0], rt, module)?
                };
                call_rt(module, builder, rt.println_fn, &[arg])?;
            } else {
                // Multi-arg print: build a pointer array and call fdn_print_many.
                let (arr, cnt) =
                    build_ptr_array(module, rt, builder, cl_vars, local_types, args, interner)?;
                call_rt(module, builder, rt.print_many_fn, &[arr, cnt])?;
            }
            Ok(call_rt(module, builder, rt.box_nothing, &[])?.unwrap())
        }

        "input" => {
            let prompt = if args.is_empty() {
                call_rt(module, builder, rt.box_nothing, &[])?.unwrap()
            } else {
                lower_operand_boxed(builder, cl_vars, local_types, &args[0], rt, module)?
            };
            Ok(call_rt(module, builder, rt.input_fn, &[prompt])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)))
        }

        "len" => {
            let arg = lower_operand_boxed(builder, cl_vars, local_types, &args[0], rt, module)?;
            let raw = call_rt(module, builder, rt.len_fn, &[arg])?
                .unwrap_or_else(|| builder.ins().iconst(I64, 0));
            // When the destination is a native integer (e.g. `_2: int = len(x)`),
            // return the raw i64 so arithmetic on the result stays in native form.
            // In all other cases (Dynamic, unknown), box it so pointer-typed uses
            // (SetIndex, list_push, etc.) receive a proper *mut FidanValue.
            if matches!(result_ty, MirTy::Integer) {
                Ok(raw)
            } else {
                Ok(call_rt(module, builder, rt.box_int, &[raw])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)))
            }
        }

        "type" => {
            let arg = lower_operand_boxed(builder, cl_vars, local_types, &args[0], rt, module)?;
            Ok(call_rt(module, builder, rt.type_name, &[arg])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)))
        }

        "assert" => {
            let cond = lower_operand(builder, cl_vars, &args[0]);
            let cond_i8 = widen_to_i8(builder, module, rt, cond, local_types, &args[0])?;
            let msg = if args.len() > 1 {
                lower_operand_boxed(builder, cl_vars, local_types, &args[1], rt, module)?
            } else {
                let (p, l) = str_const(module, builder, "assertion failed")?;
                call_rt(module, builder, rt.box_str, &[p, l])?.unwrap()
            };
            let cond_i64 = builder.ins().uextend(I64, cond_i8);
            call_rt(module, builder, rt.assert_fn, &[cond_i64, msg])?;
            Ok(call_rt(module, builder, rt.box_nothing, &[])?.unwrap())
        }

        "assertEq" | "assert_eq" | "assertNe" | "assert_ne" => {
            let (mp, ml) = str_const(module, builder, "test")?;
            let (fp, fl) = str_const(module, builder, name)?;
            let (arr, cnt) =
                build_ptr_array(module, rt, builder, cl_vars, local_types, args, interner)?;
            Ok(
                call_rt(module, builder, rt.stdlib_call, &[mp, ml, fp, fl, arr, cnt])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)),
            )
        }

        "panic" => {
            let msg = if args.is_empty() {
                call_rt(module, builder, rt.box_nothing, &[])?.unwrap()
            } else {
                lower_operand_boxed(builder, cl_vars, local_types, &args[0], rt, module)?
            };
            call_rt(module, builder, rt.panic_fn, &[msg])?;
            builder.ins().trap(TrapCode::unwrap_user(3));
            Ok(builder.ins().iconst(PTR_TY, 0)) // unreachable placeholder
        }

        "append" => {
            let list = lower_operand_boxed(builder, cl_vars, local_types, &args[0], rt, module)?;
            let val = lower_operand_boxed(builder, cl_vars, local_types, &args[1], rt, module)?;
            call_rt(module, builder, rt.list_push, &[list, val])?;
            Ok(call_rt(module, builder, rt.box_nothing, &[])?.unwrap())
        }

        "Shared" => {
            let inner = if args.is_empty() {
                call_rt(module, builder, rt.box_nothing, &[])?.unwrap()
            } else {
                lower_operand_boxed(builder, cl_vars, local_types, &args[0], rt, module)?
            };
            Ok(call_rt(module, builder, rt.make_shared, &[inner])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)))
        }
        "string" | "str" => {
            let arg = if args.is_empty() {
                call_rt(module, builder, rt.box_nothing, &[])?.unwrap()
            } else {
                lower_operand_boxed(builder, cl_vars, local_types, &args[0], rt, module)?
            };
            let boxed = call_rt(module, builder, rt.to_string, &[arg])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
            coerce_boxed_call_result(module, rt, builder, boxed, result_ty)
        }

        "integer" | "int" => {
            let arg = if args.is_empty() {
                call_rt(module, builder, rt.box_nothing, &[])?.unwrap()
            } else {
                lower_operand_boxed(builder, cl_vars, local_types, &args[0], rt, module)?
            };
            let boxed = call_rt(module, builder, rt.to_integer, &[arg])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
            coerce_boxed_call_result(module, rt, builder, boxed, result_ty)
        }

        "float" => {
            let arg = if args.is_empty() {
                call_rt(module, builder, rt.box_nothing, &[])?.unwrap()
            } else {
                lower_operand_boxed(builder, cl_vars, local_types, &args[0], rt, module)?
            };
            let boxed = call_rt(module, builder, rt.to_float, &[arg])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
            coerce_boxed_call_result(module, rt, builder, boxed, result_ty)
        }

        "boolean" | "bool" => {
            let arg = if args.is_empty() {
                call_rt(module, builder, rt.box_nothing, &[])?.unwrap()
            } else {
                lower_operand_boxed(builder, cl_vars, local_types, &args[0], rt, module)?
            };
            let boxed = call_rt(module, builder, rt.to_boolean, &[arg])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
            coerce_boxed_call_result(module, rt, builder, boxed, result_ty)
        }

        _ => {
            // Unknown: route through stdlib_call.
            let (mp, ml) = str_const(module, builder, "__builtin__")?;
            let (fp, fl) = str_const(module, builder, name)?;
            let (arr, cnt) =
                build_ptr_array(module, rt, builder, cl_vars, local_types, args, interner)?;
            Ok(
                call_rt(module, builder, rt.stdlib_call, &[mp, ml, fp, fl, arr, cnt])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)),
            )
        }
    }
}

// ── String interpolation ───────────────────────────────────────────────────────

fn lower_string_interp(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    builder: &mut FunctionBuilder<'_>,
    cl_vars: &[Variable],
    local_types: &HashMap<u32, MirTy>,
    parts: &[MirStringPart],
    _interner: &SymbolInterner,
) -> Result<cranelift_codegen::ir::Value> {
    // Build a stack-allocated array of ptr values.
    let n = parts.len() as i64;
    let slot = builder.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
        cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
        (n * 8) as u32,
        3, // 8-byte aligned
    ));
    for (i, part) in parts.iter().enumerate() {
        let offset = (i as i32) * 8;
        let boxed = match part {
            MirStringPart::Literal(s) => {
                let (p, l) = str_const(module, builder, s)?;
                call_rt(module, builder, rt.box_str, &[p, l])?.unwrap()
            }
            MirStringPart::Operand(op) => {
                let v = lower_operand_boxed(builder, cl_vars, local_types, op, rt, module)?;
                call_rt(module, builder, rt.to_string, &[v])?.unwrap()
            }
        };
        builder.ins().stack_store(boxed, slot, offset);
    }
    let arr_ptr = builder.ins().stack_addr(PTR_TY, slot, 0);
    let count = builder.ins().iconst(I64, n);
    Ok(call_rt(module, builder, rt.str_interp, &[arr_ptr, count])?
        .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)))
}

// ── C main() entry ─────────────────────────────────────────────────────────────

/// Emit one trampoline per MirFunction.
/// Each trampoline has the uniform C-ABI signature
///   `(args_ptr: *const *mut FidanValue, args_cnt: i64) -> *mut FidanValue`
/// so that `fdn_call_dynamic` can call any Fidan function generically.
/// It unboxes each argument according to the function's typed parameter list,
/// calls the real function, and boxes the result back before returning.
fn emit_trampoline_default_value(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    builder: &mut FunctionBuilder<'_>,
    default: Option<&MirLit>,
    ty: &MirTy,
) -> Result<cranelift_codegen::ir::Value> {
    if let Some(lit) = default {
        let value = match lit {
            MirLit::Int(n) => builder.ins().iconst(I64, *n),
            MirLit::Float(f) => builder.ins().f64const(*f),
            MirLit::Bool(b) => builder.ins().iconst(I8, i64::from(*b)),
            MirLit::Str(s) => {
                let (p, l) = str_const(module, builder, s)?;
                call_rt(module, builder, rt.box_str, &[p, l])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0))
            }
            MirLit::Nothing => call_rt(module, builder, rt.box_nothing, &[])?
                .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)),
            _ => builder.ins().iconst(PTR_TY, 0),
        };
        return Ok(value);
    }

    let value = match ty {
        MirTy::Integer => builder.ins().iconst(I64, 0),
        MirTy::Float => builder.ins().f64const(0.0),
        MirTy::Boolean => builder.ins().iconst(I8, 0),
        MirTy::Handle => builder.ins().iconst(I64, 0),
        _ => call_rt(module, builder, rt.box_nothing, &[])?
            .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)),
    };
    Ok(value)
}

fn emit_trampolines(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    fn_ids: &[cranelift_module::FuncId],
    ctx: &mut Context,
    builder_ctx: &mut FunctionBuilderContext,
    program: &MirProgram,
) -> Result<Vec<cranelift_module::FuncId>> {
    let mut tramp_ids = Vec::with_capacity(program.functions.len());

    for mf in &program.functions {
        let mut sig = module.make_signature();
        sig.params.push(AbiParam::new(PTR_TY)); // args_ptr: *const *mut FidanValue
        sig.params.push(AbiParam::new(I64)); // args_cnt: i64
        sig.returns.push(AbiParam::new(PTR_TY)); // *mut FidanValue

        let tramp_name = format!("fdn_trampoline_{}", mf.id.0);
        let tramp_id = module
            .declare_function(&tramp_name, Linkage::Local, &sig)
            .with_context(|| format!("declaring trampoline {tramp_name}"))?;

        ctx.func = Function::with_name_signature(UserFuncName::testcase(&tramp_name), sig);
        let mut builder = FunctionBuilder::new(&mut ctx.func, builder_ctx);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);

        let args_ptr = builder.block_params(entry)[0];
        let args_cnt = builder.block_params(entry)[1];

        let real_fn_id = fn_ids[mf.id.0 as usize];
        let real_fn_ref = module.declare_func_in_func(real_fn_id, builder.func);

        // Unbox each positional argument, but honor omitted optional/default params
        // instead of reading past the caller's provided argument list.
        let mut call_args: Vec<cranelift_codegen::ir::Value> = Vec::new();
        for (j, param) in mf.params.iter().enumerate() {
            let have_arg = builder
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThan, args_cnt, j as i64);
            let present_block = builder.create_block();
            let missing_block = builder.create_block();
            let cont_block = builder.create_block();
            let param_cl_ty = mir_ty_to_cl(&param.ty);
            builder.append_block_param(cont_block, param_cl_ty);
            builder
                .ins()
                .brif(have_arg, present_block, &[], missing_block, &[]);

            builder.switch_to_block(present_block);
            let offset = (j as i32) * 8;
            let raw = builder
                .ins()
                .load(PTR_TY, MemFlags::new(), args_ptr, offset);
            let present_val = match &param.ty {
                MirTy::Integer => {
                    let r = module.declare_func_in_func(rt.unbox_int, builder.func);
                    let inst = builder.ins().call(r, &[raw]);
                    builder.inst_results(inst)[0]
                }
                MirTy::Float => {
                    let r = module.declare_func_in_func(rt.unbox_float, builder.func);
                    let inst = builder.ins().call(r, &[raw]);
                    builder.inst_results(inst)[0]
                }
                MirTy::Boolean => {
                    let r = module.declare_func_in_func(rt.unbox_bool, builder.func);
                    let inst = builder.ins().call(r, &[raw]);
                    builder.inst_results(inst)[0]
                }
                MirTy::Handle => {
                    let r = module.declare_func_in_func(rt.unbox_handle, builder.func);
                    let inst = builder.ins().call(r, &[raw]);
                    builder.inst_results(inst)[0]
                }
                _ => raw,
            };
            builder.ins().jump(
                cont_block,
                &[cranelift_codegen::ir::BlockArg::Value(present_val)],
            );

            builder.switch_to_block(missing_block);
            let missing_val = emit_trampoline_default_value(
                module,
                rt,
                &mut builder,
                param.default.as_ref(),
                &param.ty,
            )?;
            builder.ins().jump(
                cont_block,
                &[cranelift_codegen::ir::BlockArg::Value(missing_val)],
            );

            builder.switch_to_block(cont_block);
            let val = builder.block_params(cont_block)[0];
            call_args.push(val);
        }

        // Call the real function.
        let call_inst = builder.ins().call(real_fn_ref, &call_args);
        let call_results: Vec<_> = builder.inst_results(call_inst).to_vec();

        // Box the result if it is a scalar; return a *mut FidanValue.
        let boxed = if call_results.is_empty() {
            let r = module.declare_func_in_func(rt.box_nothing, builder.func);
            let inst = builder.ins().call(r, &[]);
            builder.inst_results(inst)[0]
        } else {
            let raw_ret = call_results[0];
            let eff_ty = effective_return_ty(mf);
            match &eff_ty {
                MirTy::Integer => {
                    let r = module.declare_func_in_func(rt.box_int, builder.func);
                    let inst = builder.ins().call(r, &[raw_ret]);
                    builder.inst_results(inst)[0]
                }
                MirTy::Float => {
                    let r = module.declare_func_in_func(rt.box_float, builder.func);
                    let inst = builder.ins().call(r, &[raw_ret]);
                    builder.inst_results(inst)[0]
                }
                MirTy::Boolean => {
                    let r = module.declare_func_in_func(rt.box_bool, builder.func);
                    let inst = builder.ins().call(r, &[raw_ret]);
                    builder.inst_results(inst)[0]
                }
                MirTy::Handle => {
                    let r = module.declare_func_in_func(rt.box_handle, builder.func);
                    let inst = builder.ins().call(r, &[raw_ret]);
                    builder.inst_results(inst)[0]
                }
                MirTy::Nothing | MirTy::Error => {
                    let r = module.declare_func_in_func(rt.box_nothing, builder.func);
                    let inst = builder.ins().call(r, &[]);
                    builder.inst_results(inst)[0]
                }
                _ => raw_ret,
            }
        };

        builder.ins().return_(&[boxed]);
        builder.seal_all_blocks();
        builder.finalize();

        module
            .define_function(tramp_id, ctx)
            .with_context(|| format!("defining trampoline {tramp_name}"))?;
        module.clear_context(ctx);
        tramp_ids.push(tramp_id);
    }

    Ok(tramp_ids)
}

#[allow(clippy::too_many_arguments)]
fn emit_c_main(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    fn_ids: &[cranelift_module::FuncId],
    trampoline_ids: &[cranelift_module::FuncId],
    ctx: &mut Context,
    builder_ctx: &mut FunctionBuilderContext,
    program: &MirProgram,
    interner: &SymbolInterner,
) -> Result<()> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(I32));
    sig.params.push(AbiParam::new(PTR_TY));
    sig.returns.push(AbiParam::new(I32));
    ctx.func = Function::with_name_signature(UserFuncName::testcase("main"), sig.clone());

    let mut builder = FunctionBuilder::new(&mut ctx.func, builder_ctx);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);

    // ── Initialise the dynamic function dispatch table ─────────────────────
    let n_fns = program.functions.len() as i64;
    if n_fns > 0 {
        let init_ref = module.declare_func_in_func(rt.fn_table_init, builder.func);
        let cnt = builder.ins().iconst(I64, n_fns);
        builder.ins().call(init_ref, &[cnt]);

        let set_ref = module.declare_func_in_func(rt.fn_table_set, builder.func);
        for (i, &tramp_id) in trampoline_ids.iter().enumerate() {
            let tramp_ref = module.declare_func_in_func(tramp_id, builder.func);
            let idx = builder.ins().iconst(I64, i as i64);
            let addr = builder.ins().func_addr(PTR_TY, tramp_ref);
            builder.ins().call(set_ref, &[idx, addr]);
        }

        // ── Register function names for user-namespace dispatch ────────────
        let reg_ref = module.declare_func_in_func(rt.fn_name_register, builder.func);
        for (i, mf) in program.functions.iter().enumerate() {
            // Skip the top-level init function (index 0) — it has no user-visible name.
            if i == 0 {
                continue;
            }
            let name = interner.resolve(mf.name);
            let name_str = name.as_ref();
            let (np, nl) = str_const(module, &mut builder, name_str)?;
            let idx = builder.ins().iconst(I64, i as i64);
            builder.ins().call(reg_ref, &[np, nl, idx]);
        }
    }

    // ── Call fdn_init (function id 0 = top-level init function) ───────────
    // Only the top-level code runs automatically, exactly like the interpreter.
    // `action main` is just a definition; a call from top-level code is needed
    // to execute it — consistent with Python-style semantics.
    if !fn_ids.is_empty() {
        let init_ref = module.declare_func_in_func(fn_ids[0], builder.func);
        builder.ins().call(init_ref, &[]);
        let has_exception = call_rt(module, &mut builder, rt.has_exception, &[])?
            .unwrap_or_else(|| builder.ins().iconst(I8, 0));
        let ok_block = builder.create_block();
        let exn_block = builder.create_block();
        builder
            .ins()
            .brif(has_exception, exn_block, &[], ok_block, &[]);

        builder.switch_to_block(exn_block);
        let exn = call_rt(module, &mut builder, rt.catch_exception, &[])?
            .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0));
        call_rt(module, &mut builder, rt.throw_unhandled, &[exn])?;
        builder.ins().trap(TrapCode::unwrap_user(2));

        builder.switch_to_block(ok_block);
    }

    let zero = builder.ins().iconst(I32, 0);
    builder.ins().return_(&[zero]);
    builder.seal_all_blocks();
    builder.finalize();

    let mut main_sig = module.make_signature();
    main_sig.params.push(AbiParam::new(I32));
    main_sig.params.push(AbiParam::new(PTR_TY));
    main_sig.returns.push(AbiParam::new(I32));
    let main_id = module.declare_function("main", Linkage::Export, &main_sig)?;
    module.define_function(main_id, ctx)?;
    module.clear_context(ctx);
    Ok(())
}

// ── Phi argument collection ────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn collect_phi_args(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    builder: &mut FunctionBuilder<'_>,
    cl_vars: &[Variable],
    local_types: &HashMap<u32, MirTy>,
    mf: &MirFunction,
    from_bi: usize,
    to_bi: usize,
    interner: &SymbolInterner,
) -> Result<Vec<BlockArg>> {
    let target_bb = &mf.blocks[to_bi];
    let mut result = Vec::new();
    for phi in &target_bb.phis {
        // Determine the expected Cranelift type of this phi's block param.
        let phi_result_ty = local_types
            .get(&phi.result.0)
            .cloned()
            .unwrap_or(MirTy::Dynamic);
        let _phi_result_cl_ty = mir_ty_to_cl(&phi_result_ty);

        // Find the operand for `from_bi` predecessor.
        let op = phi
            .operands
            .iter()
            .find(|(pred, _)| pred.0 as usize == from_bi)
            .map(|(_, op)| op.clone());

        // Get the raw value and its MIR type.
        let (val, op_mir_ty) = match op {
            Some(Operand::Local(lid)) => {
                let v = builder.use_var(cl_vars[lid.0 as usize]);
                let ty = local_types.get(&lid.0).cloned().unwrap_or(MirTy::Dynamic);
                (v, ty)
            }
            Some(Operand::Const(MirLit::Int(n))) => (builder.ins().iconst(I64, n), MirTy::Integer),
            Some(Operand::Const(MirLit::Float(f))) => (builder.ins().f64const(f), MirTy::Float),
            Some(Operand::Const(MirLit::Bool(b))) => {
                (builder.ins().iconst(I8, b as i64), MirTy::Boolean)
            }
            Some(Operand::Const(lit)) => (
                lower_lit(module, builder, rt, &lit, interner)?,
                MirTy::Dynamic,
            ),
            None => (builder.ins().iconst(PTR_TY, 0), MirTy::Dynamic),
        };

        // Coerce the value to match the phi block-param type.
        // The most important case: phi expects a boxed pointer (Dynamic/PTR_TY)
        // but the incoming operand is a native scalar (Integer/Float/Boolean).
        let coerced = if matches!(phi_result_ty, MirTy::Dynamic) && is_scalar(&op_mir_ty) {
            match op_mir_ty {
                MirTy::Integer => call_rt(module, builder, rt.box_int, &[val])?.unwrap_or(val),
                MirTy::Float => call_rt(module, builder, rt.box_float, &[val])?.unwrap_or(val),
                MirTy::Boolean => call_rt(module, builder, rt.box_bool, &[val])?.unwrap_or(val),
                MirTy::Handle => call_rt(module, builder, rt.box_handle, &[val])?.unwrap_or(val),
                _ => val,
            }
        } else if is_scalar(&phi_result_ty) && matches!(op_mir_ty, MirTy::Dynamic) {
            // Reverse: phi expects a native scalar but operand is a boxed Dynamic pointer.
            match phi_result_ty {
                MirTy::Integer => call_rt(module, builder, rt.unbox_int, &[val])?.unwrap_or(val),
                MirTy::Float => call_rt(module, builder, rt.unbox_float, &[val])?.unwrap_or(val),
                MirTy::Boolean => call_rt(module, builder, rt.unbox_bool, &[val])?.unwrap_or(val),
                MirTy::Handle => call_rt(module, builder, rt.unbox_handle, &[val])?.unwrap_or(val),
                _ => val,
            }
        } else {
            val
        };

        result.push(coerced.into());
    }
    Ok(result)
}

// ── Local-type map ─────────────────────────────────────────────────────────────

fn build_local_type_map(
    mf: &MirFunction,
    program: &MirProgram,
    interner: &SymbolInterner,
) -> HashMap<u32, MirTy> {
    collect_effective_local_types(mf, program, |symbol| {
        Some(interner.resolve(symbol).to_string())
    })
}

// ── Small helpers ──────────────────────────────────────────────────────────────

/// Lower an operand to a Cranelift value (no boxing).
fn lower_operand(
    builder: &mut FunctionBuilder<'_>,
    cl_vars: &[Variable],
    op: &Operand,
) -> cranelift_codegen::ir::Value {
    match op {
        Operand::Local(lid) => builder.use_var(cl_vars[lid.0 as usize]),
        Operand::Const(MirLit::Int(n)) => builder.ins().iconst(I64, *n),
        Operand::Const(MirLit::Float(f)) => builder.ins().f64const(*f),
        Operand::Const(MirLit::Bool(b)) => builder.ins().iconst(I8, *b as i64),
        Operand::Const(_) => builder.ins().iconst(PTR_TY, 0), // non-scalar consts handled by lower_lit
    }
}

/// Box a scalar operand into a heap FidanValue pointer if needed.
fn lower_operand_boxed(
    builder: &mut FunctionBuilder<'_>,
    cl_vars: &[Variable],
    local_types: &HashMap<u32, MirTy>,
    op: &Operand,
    rt: &RuntimeDecls,
    module: &mut ObjectModule,
) -> Result<cranelift_codegen::ir::Value> {
    // Non-scalar literal constants (strings, nothing, function refs, etc.) must be
    // materialized and heap-boxed — lower_operand() yields null (iconst 0) for these.
    if let Operand::Const(lit) = op {
        match lit {
            MirLit::Str(s) => {
                let (p, l) = str_const(module, builder, s)?;
                return Ok(call_rt(module, builder, rt.box_str, &[p, l])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)));
            }
            MirLit::Nothing => {
                return Ok(call_rt(module, builder, rt.box_nothing, &[])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)));
            }
            MirLit::FunctionRef(id) => {
                let v = builder.ins().iconst(I64, *id as i64);
                return Ok(call_rt(module, builder, rt.box_fn_ref, &[v])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)));
            }
            MirLit::Namespace(m) => {
                let (p, l) = str_const(module, builder, m)?;
                return Ok(call_rt(module, builder, rt.box_namespace, &[p, l])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)));
            }
            MirLit::EnumType(m) => {
                let (p, l) = str_const(module, builder, m)?;
                return Ok(call_rt(module, builder, rt.box_enum_type, &[p, l])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)));
            }
            MirLit::ClassType(m) => {
                let (p, l) = str_const(module, builder, m)?;
                return Ok(call_rt(module, builder, rt.box_class_type, &[p, l])?
                    .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)));
            }
            MirLit::StdlibFn {
                module: mod_name,
                name,
            } => {
                let (mp, ml) = str_const(module, builder, mod_name)?;
                let (np, nl) = str_const(module, builder, name)?;
                return Ok(
                    call_rt(module, builder, rt.box_stdlib_fn, &[mp, ml, np, nl])?
                        .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)),
                );
            }
            MirLit::Int(_) | MirLit::Float(_) | MirLit::Bool(_) => {} // handled below
        }
    }
    let val = lower_operand(builder, cl_vars, op);
    let ty = operand_mir_ty(local_types, op);
    match ty {
        MirTy::Integer => Ok(call_rt(module, builder, rt.box_int, &[val])?.unwrap()),
        MirTy::Float => Ok(call_rt(module, builder, rt.box_float, &[val])?.unwrap()),
        MirTy::Boolean => Ok(call_rt(module, builder, rt.box_bool, &[val])?.unwrap()),
        MirTy::Handle => Ok(call_rt(module, builder, rt.box_handle, &[val])?.unwrap()),
        // Error type: infer boxing from the actual Cranelift value type
        // (happens for inline expressions like `{1 + 2 * 3}` whose MIR type is <error>)
        MirTy::Error => {
            let vty = builder.func.dfg.value_type(val);
            if vty == I64 {
                Ok(call_rt(module, builder, rt.box_int, &[val])?.unwrap())
            } else if vty == F64 {
                Ok(call_rt(module, builder, rt.box_float, &[val])?.unwrap())
            } else if vty == I8 {
                Ok(call_rt(module, builder, rt.box_bool, &[val])?.unwrap())
            } else {
                Ok(val) // already a pointer
            }
        }
        _ => Ok(val), // already a pointer
    }
}

fn store_local_from_boxed(
    builder: &mut FunctionBuilder<'_>,
    cl_vars: &[Variable],
    local_types: &HashMap<u32, MirTy>,
    local: LocalId,
    boxed: cranelift_codegen::ir::Value,
    rt: &RuntimeDecls,
    module: &mut ObjectModule,
) -> Result<()> {
    let value = match local_types.get(&local.0) {
        Some(MirTy::Integer) => call_rt(module, builder, rt.unbox_int, &[boxed])?.unwrap_or(boxed),
        Some(MirTy::Float) => call_rt(module, builder, rt.unbox_float, &[boxed])?.unwrap_or(boxed),
        Some(MirTy::Boolean) => call_rt(module, builder, rt.unbox_bool, &[boxed])?.unwrap_or(boxed),
        Some(MirTy::Handle) => {
            call_rt(module, builder, rt.unbox_handle, &[boxed])?.unwrap_or(boxed)
        }
        _ => call_rt(module, builder, rt.clone_any, &[boxed])?
            .unwrap_or_else(|| builder.ins().iconst(PTR_TY, 0)),
    };
    builder.def_var(cl_vars[local.0 as usize], value);
    Ok(())
}

fn zero_value_for_local(
    builder: &mut FunctionBuilder<'_>,
    ty: Option<&MirTy>,
) -> cranelift_codegen::ir::Value {
    match ty {
        Some(MirTy::Integer) | Some(MirTy::Handle) => builder.ins().iconst(I64, 0),
        Some(MirTy::Float) => builder.ins().f64const(0.0),
        Some(MirTy::Boolean) => builder.ins().iconst(I8, 0),
        _ => builder.ins().iconst(PTR_TY, 0),
    }
}

/// Same as `lower_operand_boxed` (we always want a pointer for C-ABI calls).
fn lower_operand_as_ptr(
    builder: &mut FunctionBuilder<'_>,
    cl_vars: &[Variable],
    local_types: &HashMap<u32, MirTy>,
    op: &Operand,
    rt: &RuntimeDecls,
    module: &mut ObjectModule,
) -> Result<cranelift_codegen::ir::Value> {
    if let Operand::Local(local) = op
        && matches!(local_types.get(&local.0), Some(MirTy::Error))
    {
        let raw = builder.use_var(cl_vars[local.0 as usize]);
        let raw_ty = builder.func.dfg.value_type(raw);
        return if raw_ty == F64 {
            Ok(call_rt(module, builder, rt.box_float, &[raw])?.unwrap_or(raw))
        } else if raw_ty == I8 {
            Ok(call_rt(module, builder, rt.box_bool, &[raw])?.unwrap_or(raw))
        } else {
            Ok(raw)
        };
    }

    lower_operand_boxed(builder, cl_vars, local_types, op, rt, module)
}

/// Coerce an operand to the expected parameter type.
fn lower_operand_coerced(
    builder: &mut FunctionBuilder<'_>,
    cl_vars: &[Variable],
    local_types: &HashMap<u32, MirTy>,
    op: &Operand,
    param_ty: &MirTy,
    rt: &RuntimeDecls,
    module: &mut ObjectModule,
) -> Result<cranelift_codegen::ir::Value> {
    if is_scalar(param_ty) {
        let op_mir_ty = operand_mir_ty(local_types, op);
        let raw = lower_operand(builder, cl_vars, op);
        if matches!(op_mir_ty, MirTy::Dynamic) {
            return match param_ty {
                MirTy::Integer => {
                    Ok(call_rt(module, builder, rt.unbox_int, &[raw])?.unwrap_or(raw))
                }
                MirTy::Float => {
                    Ok(call_rt(module, builder, rt.unbox_float, &[raw])?.unwrap_or(raw))
                }
                MirTy::Boolean => {
                    Ok(call_rt(module, builder, rt.unbox_bool, &[raw])?.unwrap_or(raw))
                }
                MirTy::Handle => {
                    Ok(call_rt(module, builder, rt.unbox_handle, &[raw])?.unwrap_or(raw))
                }
                _ => Ok(raw),
            };
        }
        let expected_cl_ty = mir_ty_to_cl(param_ty);
        let actual_cl_ty = builder.func.dfg.value_type(raw);
        if actual_cl_ty != expected_cl_ty {
            coerce_value(builder, module, rt, raw, actual_cl_ty, expected_cl_ty)
        } else {
            Ok(raw)
        }
    } else {
        lower_operand_boxed(builder, cl_vars, local_types, op, rt, module)
    }
}

fn widen_to_i64(
    builder: &mut FunctionBuilder<'_>,
    val: cranelift_codegen::ir::Value,
    _local_types: &HashMap<u32, MirTy>,
    _op: &Operand,
) -> cranelift_codegen::ir::Value {
    let ty = builder.func.dfg.value_type(val);
    if ty == I8 {
        builder.ins().uextend(I64, val)
    } else if ty == F64 {
        let zero = builder.ins().f64const(0.0);
        let flag = builder.ins().fcmp(FloatCC::NotEqual, val, zero);
        builder.ins().uextend(I64, flag)
    } else {
        val
    }
}

fn widen_to_i8(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    val: cranelift_codegen::ir::Value,
    local_types: &HashMap<u32, MirTy>,
    op: &Operand,
) -> Result<cranelift_codegen::ir::Value> {
    let ty = builder.func.dfg.value_type(val);
    if ty == I8 {
        return Ok(val);
    }

    if ty == F64 {
        let zero = builder.ins().f64const(0.0);
        return Ok(builder.ins().fcmp(FloatCC::NotEqual, val, zero));
    }

    match operand_mir_ty(local_types, op) {
        MirTy::Integer | MirTy::Handle => Ok(builder.ins().icmp_imm(IntCC::NotEqual, val, 0)),
        MirTy::Dynamic
        | MirTy::String
        | MirTy::List(_)
        | MirTy::Dict(_, _)
        | MirTy::HashSet(_)
        | MirTy::Tuple(_)
        | MirTy::Object(_)
        | MirTy::Enum(_)
        | MirTy::Shared(_)
        | MirTy::WeakShared(_)
        | MirTy::Pending(_)
        | MirTy::Function
        | MirTy::Nothing
        | MirTy::Error => Ok(call_rt(module, builder, rt.truthy, &[val])?.unwrap_or(val)),
        MirTy::Boolean => Ok(builder.ins().icmp_imm(IntCC::NotEqual, val, 0)),
        MirTy::Float => {
            let zero = builder.ins().f64const(0.0);
            Ok(builder.ins().fcmp(FloatCC::NotEqual, val, zero))
        }
    }
}

fn operand_ty(
    _cl_vars: &[Variable],
    local_types: &HashMap<u32, MirTy>,
    op: &Operand,
) -> cranelift_codegen::ir::Type {
    mir_ty_to_cl(&operand_mir_ty(local_types, op))
}

fn operand_mir_ty(local_types: &HashMap<u32, MirTy>, op: &Operand) -> MirTy {
    match op {
        Operand::Local(l) => local_types.get(&l.0).cloned().unwrap_or(MirTy::Dynamic),
        Operand::Const(MirLit::Int(_)) => MirTy::Integer,
        Operand::Const(MirLit::Float(_)) => MirTy::Float,
        Operand::Const(MirLit::Bool(_)) => MirTy::Boolean,
        _ => MirTy::Dynamic,
    }
}

/// Emit a string constant into the object's `.rodata` and return (ptr_val, len_val).
fn str_const(
    module: &mut ObjectModule,
    builder: &mut FunctionBuilder<'_>,
    s: &str,
) -> Result<(cranelift_codegen::ir::Value, cranelift_codegen::ir::Value)> {
    let mut desc = DataDescription::new();
    // null-terminated for C compatibility
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    desc.define(bytes.into_boxed_slice());
    let data_id = module
        .declare_anonymous_data(false, false)
        .context("declaring string constant")?;
    module
        .define_data(data_id, &desc)
        .context("defining string constant")?;
    let gref = module.declare_data_in_func(data_id, builder.func);
    let ptr = builder.ins().global_value(PTR_TY, gref);
    let len = builder.ins().iconst(I64, s.len() as i64);
    Ok((ptr, len))
}

/// Build a stack-allocated array of boxed ptr values; return (array_ptr, count).
fn build_ptr_array(
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    builder: &mut FunctionBuilder<'_>,
    cl_vars: &[Variable],
    local_types: &HashMap<u32, MirTy>,
    args: &[Operand],
    _interner: &SymbolInterner,
) -> Result<(cranelift_codegen::ir::Value, cranelift_codegen::ir::Value)> {
    let n = args.len() as i64;
    if n == 0 {
        let null = builder.ins().iconst(PTR_TY, 0);
        let zero = builder.ins().iconst(I64, 0);
        return Ok((null, zero));
    }
    let slot = builder.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
        cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
        (n * 8) as u32,
        3u8,
    ));
    for (i, op) in args.iter().enumerate() {
        let v = lower_operand_boxed(builder, cl_vars, local_types, op, rt, module)?;
        builder.ins().stack_store(v, slot, (i as i32) * 8);
    }
    let ptr = builder.ins().stack_addr(PTR_TY, slot, 0);
    let cnt = builder.ins().iconst(I64, n);
    Ok((ptr, cnt))
}

/// Coerce a Cranelift value from `actual_ty` to `expected_ty` by boxing or
/// unboxing through the runtime.  Only the cases that occur naturally in Fidan
/// code generation are handled; any unexpected pairing returns the value as-is.
fn coerce_value(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    rt: &RuntimeDecls,
    val: cranelift_codegen::ir::Value,
    actual_ty: cranelift_codegen::ir::Type,
    expected_ty: cranelift_codegen::ir::Type,
) -> Result<cranelift_codegen::ir::Value> {
    use cranelift_codegen::ir::types::{F64, I8, I64};
    match (actual_ty, expected_ty) {
        // Boxed pointer → float scalar: unbox
        (a, F64) if a == I64 => {
            let r = module.declare_func_in_func(rt.unbox_float, builder.func);
            let inst = builder.ins().call(r, &[val]);
            Ok(builder.inst_results(inst)[0])
        }
        // Float scalar → boxed pointer: box
        (F64, e) if e == I64 => {
            let r = module.declare_func_in_func(rt.box_float, builder.func);
            let inst = builder.ins().call(r, &[val]);
            Ok(builder.inst_results(inst)[0])
        }
        // Boxed pointer → bool scalar: unbox
        (a, I8) if a == I64 => {
            let r = module.declare_func_in_func(rt.unbox_bool, builder.func);
            let inst = builder.ins().call(r, &[val]);
            Ok(builder.inst_results(inst)[0])
        }
        // Bool scalar → boxed pointer: box
        (I8, e) if e == I64 => {
            let r = module.declare_func_in_func(rt.box_bool, builder.func);
            let inst = builder.ins().call(r, &[val]);
            Ok(builder.inst_results(inst)[0])
        }
        // Integer scalar → boxed pointer: box
        (a, e) if a == I64 && e == I64 => Ok(val), // same type, nothing to do
        // Fallback — return as-is and let Cranelift surface the error
        _ => Ok(val),
    }
}

/// Call a runtime function by its `FuncId`; return the first result value (if any).
fn call_rt(
    module: &mut ObjectModule,
    builder: &mut FunctionBuilder<'_>,
    func_id: cranelift_module::FuncId,
    args: &[cranelift_codegen::ir::Value],
) -> Result<Option<cranelift_codegen::ir::Value>> {
    let func_ref = module.declare_func_in_func(func_id, builder.func);
    let inst = builder.ins().call(func_ref, args);
    let results = builder.inst_results(inst);
    Ok(results.first().copied())
}

// ── System linker invocation ───────────────────────────────────────────────────

fn link(
    obj_path: &Path,
    output_path: &Path,
    extra_lib_dirs: &[PathBuf],
    link_dynamic: bool,
    lto: LtoMode,
    extern_link_inputs: &[String],
) -> Result<()> {
    let linker = find_linker()?;
    let runtime_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_owned()))
        .unwrap_or_else(|| PathBuf::from("."));

    let mut cmd = std::process::Command::new(&linker);

    // Decide argument style: MSVC/lld-link use `/FLAG` style; GNU (gcc,g++)
    // uses `-flag` style even on Windows.  When the linker is a full path
    // (e.g. from the component dir), check only the file stem.
    let linker_stem = std::path::Path::new(&linker)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| linker.clone());
    let is_gnu = matches!(
        linker_stem.as_str(),
        "gcc" | "g++" | "cc" | "clang" | "clang++"
    );

    if cfg!(windows) && !is_gnu {
        cmd.arg(format!("/OUT:{}", output_path.display()));
        // Tell the linker this is a console app so it includes mainCRTStartup.
        cmd.arg("/SUBSYSTEM:CONSOLE");
        cmd.arg(obj_path);
        if lto == LtoMode::Full {
            cmd.arg("/LTCG");
        }

        // Dynamically locate MSVC + Windows SDK lib dirs so the linker can
        // resolve the CRT (msvcrt.lib, vcruntime.lib, ucrt.lib) and Win32
        // imports (kernel32.lib, etc.) even outside a VS Developer prompt.
        for dir in find_msvc_lib_paths() {
            cmd.arg(format!("/LIBPATH:{}", dir.display()));
        }
        for dir in extra_lib_dirs {
            cmd.arg(format!("/LIBPATH:{}", dir.display()));
        }
        cmd.arg(format!("/LIBPATH:{}", runtime_dir.display()));
        if link_dynamic {
            let import_lib = find_dynamic_runtime_import_lib(&runtime_dir)
                .context("cannot find fidan_runtime.dll.lib — build Fidan first")?;
            cmd.arg(import_lib);
        } else {
            let lib = find_static_runtime_lib(&runtime_dir)
                .context("cannot find fidan_runtime.lib — build Fidan first")?;
            cmd.arg(&lib);
        }
        for input in extern_link_inputs {
            append_windows_link_input(&mut cmd, input);
        }
        // Always-needed Windows system libs for a Rust staticlib.
        cmd.args([
            "kernel32.lib",
            "ucrt.lib",
            "msvcrt.lib",
            "vcruntime.lib",
            "ws2_32.lib",
            "userenv.lib",
            "ntdll.lib",
            "bcrypt.lib",
            "advapi32.lib",
        ]);
    } else {
        // GNU driver on any platform — covers Linux, macOS, and Windows MinGW.
        cmd.arg("-o").arg(output_path);
        cmd.arg(obj_path);
        if lto == LtoMode::Full {
            cmd.arg("-flto=full");
        }
        for dir in extra_lib_dirs {
            cmd.arg(format!("-L{}", dir.display()));
        }
        cmd.arg(format!("-L{}", runtime_dir.display()));
        if link_dynamic {
            cmd.arg("-lfidan_runtime");
            cmd.arg(format!("-Wl,-rpath,{}", runtime_dir.display()));
            if !cfg!(target_os = "macos") {
                cmd.arg("-Wl,--enable-new-dtags");
            }
        } else {
            let lib = find_static_runtime_lib(&runtime_dir)
                .context("cannot find the Fidan runtime library — build Fidan first")?;
            cmd.arg(&lib);
        }
        for input in extern_link_inputs {
            append_unix_link_input(&mut cmd, input);
        }
        #[cfg(target_os = "linux")]
        cmd.args(["-lpthread", "-ldl", "-lm"]);
    }

    let output = cmd
        .output()
        .with_context(|| format!("failed to spawn linker `{linker}`"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = match (stdout.is_empty(), stderr.is_empty()) {
            (true, true) => String::new(),
            (false, true) => format!(": {stdout}"),
            (true, false) => format!(": {stderr}"),
            (false, false) => format!(":\n{stdout}\n{stderr}"),
        };
        bail!(
            "linker `{linker}` exited with code {:?}{}",
            output.status.code(),
            detail
        );
    }
    Ok(())
}

fn append_windows_link_input(command: &mut std::process::Command, input: &str) {
    let resolved = resolve_windows_link_input(input);
    let path_like = input.contains(std::path::MAIN_SEPARATOR)
        || input.contains('/')
        || input.contains('\\')
        || input.contains(':');
    if path_like {
        command.arg(&resolved);
        return;
    }

    let has_lib_suffix = std::path::Path::new(&resolved)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("lib"))
        .unwrap_or(false);
    if has_lib_suffix {
        command.arg(&resolved);
    } else {
        command.arg(format!("{resolved}.lib"));
    }
}

fn resolve_windows_link_input(input: &str) -> String {
    let path = Path::new(input);
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());

    if ext.as_deref() != Some("dll") {
        return input.to_owned();
    }

    let dll_lib = PathBuf::from(format!("{input}.lib"));
    if dll_lib.is_file() {
        return dll_lib.to_string_lossy().into_owned();
    }

    let import_lib = path.with_extension("lib");
    if import_lib.is_file() {
        return import_lib.to_string_lossy().into_owned();
    }

    dll_lib.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        map_target_cpu_feature_name, normalize_target_cpu_preset, resolve_windows_link_input,
        split_target_cpu_spec,
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    use target_lexicon::Architecture;

    fn unique_temp_dir() -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let suffix = COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = std::env::temp_dir().join(format!(
            "fidan_cranelift_link_input_{}_{}",
            std::process::id(),
            suffix
        ));
        std::fs::create_dir_all(&temp).expect("create temp dir");
        temp
    }

    #[test]
    fn windows_link_input_prefers_dll_lib_sidecar() {
        let temp = unique_temp_dir();
        let dll = temp.join("ffi_demo.dll");
        let dll_lib = temp.join("ffi_demo.dll.lib");
        std::fs::write(&dll, []).expect("write dll placeholder");
        std::fs::write(&dll_lib, []).expect("write import lib placeholder");

        let resolved = resolve_windows_link_input(&dll.to_string_lossy());
        assert_eq!(resolved, dll_lib.to_string_lossy());

        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn windows_link_input_falls_back_to_plain_lib_sidecar() {
        let temp = unique_temp_dir();
        let dll = temp.join("ffi_demo.dll");
        let lib = temp.join("ffi_demo.lib");
        std::fs::write(&dll, []).expect("write dll placeholder");
        std::fs::write(&lib, []).expect("write import lib placeholder");

        let resolved = resolve_windows_link_input(&dll.to_string_lossy());
        assert_eq!(resolved, lib.to_string_lossy());

        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn target_cpu_spec_splits_cpu_and_features() {
        let (cpu, features) = split_target_cpu_spec("haswell,+avx2,-bmi2").unwrap();
        assert_eq!(cpu, "haswell");
        assert_eq!(features, "+avx2,-bmi2");
    }

    #[test]
    fn x64_feature_aliases_map_to_cranelift_flags() {
        assert_eq!(
            map_target_cpu_feature_name(Architecture::X86_64, "sse4.1"),
            "has_sse41"
        );
        assert_eq!(
            map_target_cpu_feature_name(Architecture::X86_64, "cx16"),
            "has_cmpxchg16b"
        );
    }

    #[test]
    fn x64_presets_normalize_underscores() {
        assert_eq!(
            normalize_target_cpu_preset(Architecture::X86_64, "x86_64_v3"),
            "x86-64-v3"
        );
    }
}

fn append_unix_link_input(command: &mut std::process::Command, input: &str) {
    let path_like =
        input.contains(std::path::MAIN_SEPARATOR) || input.contains('/') || input.contains('\\');
    let explicit_library = [".a", ".so", ".dylib", ".tbd"]
        .iter()
        .any(|suffix| input.ends_with(suffix));
    if path_like || explicit_library {
        command.arg(input);
    } else {
        command.arg(format!("-l{input}"));
    }
}

fn strip_binary(output_path: &Path, mode: StripMode) -> Result<()> {
    let (tool, kind) = find_strip_tool()?;
    let mut cmd = std::process::Command::new(&tool);

    match kind {
        StripToolKind::Llvm | StripToolKind::Gnu => match mode {
            StripMode::Off => return Ok(()),
            StripMode::Symbols => {
                cmd.arg("--strip-unneeded");
            }
            StripMode::All => {
                cmd.arg("--strip-all");
            }
        },
        StripToolKind::MacOs => match mode {
            StripMode::Off => return Ok(()),
            StripMode::Symbols => {
                cmd.arg("-x");
            }
            StripMode::All => {
                cmd.args(["-x", "-S"]);
            }
        },
    }

    cmd.arg(output_path);
    let output = cmd
        .output()
        .with_context(|| format!("failed to launch strip tool `{}`", tool.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = match (stdout.is_empty(), stderr.is_empty()) {
            (true, true) => String::new(),
            (false, true) => format!(": {stdout}"),
            (true, false) => format!(": {stderr}"),
            (false, false) => format!(":\n{stdout}\n{stderr}"),
        };
        bail!(
            "strip tool `{}` exited with code {:?}{}",
            tool.display(),
            output.status.code(),
            detail
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StripToolKind {
    Llvm,
    Gnu,
    MacOs,
}

fn find_strip_tool() -> Result<(PathBuf, StripToolKind)> {
    let candidates: &[(&str, StripToolKind)] = if cfg!(windows) {
        &[
            ("llvm-strip.exe", StripToolKind::Llvm),
            ("strip.exe", StripToolKind::Gnu),
        ]
    } else if cfg!(target_os = "macos") {
        &[
            ("llvm-strip", StripToolKind::Llvm),
            ("strip", StripToolKind::MacOs),
        ]
    } else {
        &[
            ("llvm-strip", StripToolKind::Llvm),
            ("strip", StripToolKind::Gnu),
        ]
    };

    for &(name, kind) in candidates {
        if std::process::Command::new(name)
            .arg(if matches!(kind, StripToolKind::MacOs) {
                "-h"
            } else {
                "--version"
            })
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            return Ok((PathBuf::from(name), kind));
        }
    }

    bail!(
        "no standalone strip tool found (tried: {}); install llvm-strip or a compatible system strip",
        candidates
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn find_linker() -> Result<String> {
    // Respect explicit user override.
    if let Ok(v) = std::env::var("FIDAN_LINKER")
        && !v.is_empty()
    {
        return Ok(v);
    }

    // Check the components directory for lld (e.g. {exe_dir}/components/llvm/bin/).
    if let Some(lld) = find_component_linker() {
        return Ok(lld.to_string_lossy().into_owned());
    }

    // On Windows we prefer lld-link and fall back to plain link.exe (which
    // needs the VS Developer Prompt environment).  On non-Windows environments
    // that have MinGW/MSYS2 we accept gcc/g++ last.
    //
    // On Unix we use the C compiler driver (cc/gcc/clang) so it handles all
    // the platform-specific rpath/sysroot logic for us.
    let candidates: &[&str] = if cfg!(windows) {
        &["lld-link.exe", "link.exe", "gcc", "g++"]
    } else {
        &["cc", "gcc", "clang"]
    };

    for &c in candidates {
        if std::process::Command::new(c)
            .arg(if cfg!(windows) { "/?" } else { "--version" })
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            return Ok(c.to_owned());
        }
    }
    bail!(
        "no system linker found (tried: {}); set FIDAN_LINKER to override",
        candidates.join(", ")
    )
}

/// Returns the `lld-link` / `lld` / `clang` binary from the components
/// directory (`{exe_dir}/components/llvm/bin/`), if present.
fn find_component_linker() -> Option<PathBuf> {
    let dir = std::env::current_exe()
        .ok()?
        .parent()?
        .join("components")
        .join("llvm");
    let candidates: &[&str] = if cfg!(windows) {
        &["bin/lld-link.exe", "bin/clang.exe"]
    } else {
        &["bin/ld.lld", "bin/lld", "bin/clang"]
    };
    for &rel in candidates {
        let p = dir.join(rel);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn find_static_runtime_lib(dir: &Path) -> Option<PathBuf> {
    let candidates: &[&str] = if cfg!(windows) {
        &["fidan_runtime.lib", "libfidan_runtime.lib"]
    } else {
        &["libfidan_runtime.a"]
    };
    find_latest_runtime_artifact(dir, candidates)
}

fn find_dynamic_runtime_import_lib(dir: &Path) -> Option<PathBuf> {
    if cfg!(windows) {
        find_latest_runtime_artifact(dir, &["fidan_runtime.dll.lib", "libfidan_runtime.dll.lib"])
    } else {
        None
    }
}

fn find_latest_runtime_artifact(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    let mut matches = Vec::new();
    for candidate_dir in [Some(dir), Some(&dir.join("deps"))].into_iter().flatten() {
        for &name in names {
            let path = candidate_dir.join(name);
            if let Ok(metadata) = std::fs::metadata(&path)
                && metadata.is_file()
            {
                matches.push((metadata.modified().ok(), path));
            }
        }
    }

    matches.sort_by_key(|(modified, path)| (*modified, path.clone()));
    matches.pop().map(|(_, path)| path)
}

// ── Windows: locate MSVC + Windows SDK library directories ────────────────────
//
// On Windows a Rust `staticlib` references the MSVC CRT (msvcrt.lib,
// vcruntime.lib, ucrt.lib) and Win32 APIs (kernel32.lib, …).  When the user
// runs `fidan build` outside of a VS Developer Command Prompt the `LIB`
// environment variable is not set, so the linker cannot find those libs.
//
// `find_msvc_lib_paths` discovers them dynamically:
//   1. MSVC libs   – via vswhere.exe  → <VS>\VC\Tools\MSVC\<ver>\lib\x64
//   2. Win32 UM    – via registry     → <SDK>\Lib\<ver>\um\x64
//   3. UCRT        – via registry     → <SDK>\Lib\<ver>\ucrt\x64

#[cfg(windows)]
fn find_msvc_lib_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // --- 1. MSVC compiler libs via vswhere.exe ---
    // Use -latest without -requires so we don't need a specific component ID.
    let vswhere_candidates = [
        r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe",
        r"C:\Program Files\Microsoft Visual Studio\Installer\vswhere.exe",
    ];
    for vswhere in &vswhere_candidates {
        if let Ok(out) = std::process::Command::new(vswhere)
            .args(["-latest", "-property", "installationPath"])
            .output()
            && out.status.success()
        {
            let vs_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !vs_path.is_empty() {
                let msvc_root = PathBuf::from(&vs_path).join(r"VC\Tools\MSVC");
                if let Ok(entries) = std::fs::read_dir(&msvc_root) {
                    let mut versions: Vec<PathBuf> = entries
                        .flatten()
                        .filter(|e| e.path().is_dir())
                        .map(|e| e.path())
                        .collect();
                    versions.sort();
                    if let Some(latest) = versions.last() {
                        let lib = latest.join(r"lib\x64");
                        if lib.exists() {
                            paths.push(lib);
                        }
                    }
                }
                break;
            }
        }
    }

    // --- 2 & 3. Windows SDK (UM + UCRT) via registry ---
    let sdk_root = query_registry_value(
        r"HKLM\SOFTWARE\Microsoft\Windows Kits\Installed Roots",
        "KitsRoot10",
    )
    .or_else(|| {
        query_registry_value(
            r"HKLM\SOFTWARE\WOW6432Node\Microsoft\Windows Kits\Installed Roots",
            "KitsRoot10",
        )
    });

    if let Some(root) = sdk_root {
        let lib_root = PathBuf::from(&root).join("Lib");
        if let Ok(entries) = std::fs::read_dir(&lib_root) {
            let mut versions: Vec<PathBuf> = entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.path())
                .collect();
            versions.sort();
            if let Some(latest) = versions.last() {
                let um = latest.join(r"um\x64");
                let ucrt = latest.join(r"ucrt\x64");
                if um.exists() {
                    paths.push(um);
                }
                if ucrt.exists() {
                    paths.push(ucrt);
                }
            }
        }
    }

    paths
}

#[cfg(not(windows))]
fn find_msvc_lib_paths() -> Vec<PathBuf> {
    vec![]
}

#[cfg(windows)]
fn query_registry_value(key: &str, value_name: &str) -> Option<String> {
    let out = std::process::Command::new("reg")
        .args(["query", key, "/v", value_name])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with(value_name) {
            continue;
        }
        // Format: "<value_name>    REG_SZ    <data>"
        let rest = line[value_name.len()..].trim();
        // Skip the REG type token (e.g. "REG_SZ") — it ends before the next run of spaces.
        if let Some(pos) = rest.find("    ") {
            return Some(rest[pos..].trim().to_string());
        }
    }
    None
}

enum StdlibResultValue {
    Integer(cranelift_codegen::ir::Value),
    Float(cranelift_codegen::ir::Value),
}

fn operand_stdlib_kind(op: &Operand, local_types: &HashMap<u32, MirTy>) -> StdlibValueKind {
    match op {
        Operand::Local(local) => local_types
            .get(&local.0)
            .cloned()
            .map(mir_ty_to_stdlib_kind)
            .unwrap_or(StdlibValueKind::Dynamic),
        Operand::Const(MirLit::Int(_)) => StdlibValueKind::Integer,
        Operand::Const(MirLit::Float(_)) => StdlibValueKind::Float,
        Operand::Const(MirLit::Bool(_)) => StdlibValueKind::Boolean,
        Operand::Const(MirLit::Str(_)) => StdlibValueKind::String,
        Operand::Const(MirLit::Nothing) => StdlibValueKind::Nothing,
        _ => StdlibValueKind::Dynamic,
    }
}

fn mir_ty_to_stdlib_kind(ty: MirTy) -> StdlibValueKind {
    match ty {
        MirTy::Integer => StdlibValueKind::Integer,
        MirTy::Float => StdlibValueKind::Float,
        MirTy::Boolean => StdlibValueKind::Boolean,
        MirTy::String => StdlibValueKind::String,
        MirTy::List(_) => StdlibValueKind::List,
        MirTy::Dict(_, _) => StdlibValueKind::Dict,
        MirTy::HashSet(_) => StdlibValueKind::HashSet,
        MirTy::Nothing => StdlibValueKind::Nothing,
        _ => StdlibValueKind::Dynamic,
    }
}

fn call_may_throw(
    callee: &Callee,
    args: &[Operand],
    local_types: &HashMap<u32, MirTy>,
    namespace_locals: &HashMap<LocalId, String>,
    function_throw_map: &HashMap<MirFunctionId, bool>,
    interner: &SymbolInterner,
) -> Result<bool> {
    match callee {
        Callee::Fn(function_id) => Ok(function_throw_map.get(function_id).copied().unwrap_or(true)),
        Callee::Method { receiver, method } => {
            let Some(namespace) = stdlib_namespace(receiver, namespace_locals) else {
                return Ok(true);
            };
            let method_name = interner.resolve(*method).to_string();
            let arg_kinds = args
                .iter()
                .map(|arg| operand_stdlib_kind(arg, local_types))
                .collect::<Vec<_>>();
            Ok(
                infer_stdlib_method(namespace.as_str(), method_name.as_str(), &arg_kinds)
                    .and_then(|info| info.intrinsic)
                    .is_none(),
            )
        }
        _ => Ok(true),
    }
}

fn stdlib_namespace(
    receiver: &Operand,
    namespace_locals: &HashMap<LocalId, String>,
) -> Option<String> {
    let namespace = match receiver {
        Operand::Local(local) => namespace_locals.get(local).cloned(),
        Operand::Const(MirLit::Namespace(namespace)) => Some(namespace.clone()),
        _ => None,
    }?;
    is_stdlib_module(namespace.as_str()).then_some(namespace)
}
