// fidan-mir/src/mir.rs
//
// Mid-Level Intermediate Representation.
//
// The MIR is a flat, explicit, SSA-form control-flow graph (CFG) suited for
// optimisation and code generation.
//
// Key properties:
//   • Every variable (`LocalId`) is assigned exactly once (SSA form).
//   • Control flow is represented as `BasicBlock`s with explicit `Terminator`s.
//   • All types are explicit (`MirTy` on every `Instr::Assign`).
//   • Concurrency is lowered to explicit spawn/join instructions.
//   • No HIR sugar remains — everything is flat.

use fidan_ast::{BinOp, UnOp};
use fidan_lexer::Symbol;
use fidan_source::Span;

// ── Identifiers ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FunctionId(pub u32);

// ── MIR Type ───────────────────────────────────────────────────────────────────

/// The type of a MIR local or operand.
///
/// Mirrors `FidanType` but lives in the MIR layer; may diverge over time
/// (e.g., by adding ABI-specific types for codegen).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MirTy {
    Integer,
    Float,
    Boolean,
    String,
    Nothing,
    Dynamic,
    List(Box<MirTy>),
    Dict(Box<MirTy>, Box<MirTy>),
    Tuple(Vec<MirTy>),
    Object(Symbol),
    Shared(Box<MirTy>),
    Pending(Box<MirTy>),
    Function,
    Error,
}

impl MirTy {
    pub fn is_nothing(&self) -> bool {
        matches!(self, MirTy::Nothing)
    }
}

// ── Literals ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum MirLit {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Nothing,
    /// A reference to a known function/action, used to pass actions as values.
    FunctionRef(u32),
    /// A stdlib module namespace (e.g. `"io"`, `"math"`).
    Namespace(String),
}

// ── Operands ───────────────────────────────────────────────────────────────────

/// A lightweight, copy-like reference to a value — either a local variable
/// (SSA name) or a compile-time constant.
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    Local(LocalId),
    Const(MirLit),
}

// ── Callees ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Callee {
    /// Direct function call (known at compile time).
    Fn(FunctionId),
    /// Method dispatch via vtable: `receiver.method(...)`.
    Method { receiver: Operand, method: Symbol },
    /// Call to a named built-in function (e.g. `print`, `len`).
    /// Stored as a `Symbol` so the interpreter can resolve the name.
    Builtin(Symbol),
    /// Dynamic call through a function-value operand.
    Dynamic(Operand),
}

// ── String interpolation ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum MirStringPart {
    Literal(String),
    Operand(Operand),
}

// ── Rvalues ────────────────────────────────────────────────────────────────────

/// Right-hand side of an `Instr::Assign`.
#[derive(Debug, Clone)]
pub enum Rvalue {
    /// `dest = operand`  (copy / move).
    Use(Operand),
    Binary {
        op: BinOp,
        lhs: Operand,
        rhs: Operand,
    },
    Unary {
        op: UnOp,
        operand: Operand,
    },
    NullCoalesce {
        lhs: Operand,
        rhs: Operand,
    },
    Call {
        callee: Callee,
        args: Vec<Operand>,
    },
    /// Construct an object: `TypeName(field1: v1, field2: v2, ...)`.
    Construct {
        ty: Symbol,
        fields: Vec<(Symbol, Operand)>,
    },
    List(Vec<Operand>),
    Dict(Vec<(Operand, Operand)>),
    Tuple(Vec<Operand>),
    StringInterp(Vec<MirStringPart>),
    /// A compile-time literal that needs no evaluation.
    Literal(MirLit),
    /// Read the exception that caused the current catch-block to be entered.
    /// Placed by the lowerer as the initialiser of each `catch err {` binding.
    CatchException,
}

// ── Instructions ──────────────────────────────────────────────────────────────

/// A single MIR instruction (not a terminator).
#[derive(Debug, Clone)]
pub enum Instr {
    // ── Core ──────────────────────────────────────────────────────────────────
    /// `dest: ty = rhs`  (SSA definition — each local is defined exactly once)
    Assign {
        dest: LocalId,
        ty: MirTy,
        rhs: Rvalue,
    },

    /// Function / method call whose result (if any) is stored in `dest`.
    Call {
        dest: Option<LocalId>,
        callee: Callee,
        args: Vec<Operand>,
        span: Span,
    },

    /// `object.field = value`
    SetField {
        object: Operand,
        field: Symbol,
        value: Operand,
    },
    /// `dest = object.field`
    GetField {
        dest: LocalId,
        object: Operand,
        field: Symbol,
    },

    /// `dest = object[index]`
    GetIndex {
        dest: LocalId,
        object: Operand,
        index: Operand,
    },
    /// `object[index] = value`
    SetIndex {
        object: Operand,
        index: Operand,
        value: Operand,
    },

    /// Explicit scope-exit: owned value is freed here.
    Drop { local: LocalId },

    // ── Concurrency (Phase 5.5) ───────────────────────────────────────────────
    /// Spawn a cooperative green-thread task (`concurrent` block).
    SpawnConcurrent {
        handle: LocalId,
        task_fn: FunctionId,
        args: Vec<Operand>,
    },
    /// Spawn a parallel OS-thread task (`parallel` block / `parallel action`).
    SpawnParallel {
        handle: LocalId,
        task_fn: FunctionId,
        args: Vec<Operand>,
    },
    /// Wait for ALL given join handles (end of a concurrent / parallel block).
    JoinAll { handles: Vec<LocalId> },
    /// `spawn expr` — non-blocking call, result is `Pending oftype T`.
    SpawnExpr {
        dest: LocalId,
        task_fn: FunctionId,
        args: Vec<Operand>,
    },
    /// `spawn callee(args)` where the callee is not a statically-resolved `FunctionId`.
    /// `method = Some(sym)` → method dispatch; receiver is `args[0]`.
    /// `method = None`      → dynamic function-value dispatch; fn-value is `args[0]`.
    SpawnDynamic {
        dest: LocalId,
        method: Option<Symbol>,
        args: Vec<Operand>,
    },
    /// `await pending` — block until the `Pending oftype T` resolves.
    AwaitPending { dest: LocalId, handle: Operand },
    /// `parallel for item in collection { body }` — distribute iterations.
    ParallelIter {
        collection: Operand,
        body_fn: FunctionId,
        closure_args: Vec<Operand>,
    },

    /// No-op (used as a placeholder during construction / optimisation).
    Nop,

    // ── Exception handling ────────────────────────────────────────────────────
    /// Push `catch_bb` onto the interpreter's exception-handler stack.
    /// All `Terminator::Throw`s encountered while this is active jump to
    /// `catch_bb` instead of propagating out of the function.
    PushCatch(BlockId),
    /// Pop the innermost exception handler installed by `PushCatch`.
    PopCatch,
}

// ── Terminators ────────────────────────────────────────────────────────────────

/// The final instruction of a basic block — determines the block's successor(s).
#[derive(Debug, Clone)]
pub enum Terminator {
    /// Return from the function (with optional value).
    Return(Option<Operand>),
    /// Unconditional jump.
    Goto(BlockId),
    /// Conditional branch.
    Branch {
        cond: Operand,
        then_bb: BlockId,
        else_bb: BlockId,
    },
    /// Throw an exception; unwinds to the nearest `Attempt` landing pad.
    Throw { value: Operand },
    /// Statically unreachable (e.g., after a `panic` with no catch).
    Unreachable,
}

// ── Phi nodes ─────────────────────────────────────────────────────────────────

/// SSA φ-node at the beginning of a basic block.
///
/// `result = φ(v1 from bb1, v2 from bb2, ...)`
///
/// Merged value depends on which predecessor block was taken.
#[derive(Debug, Clone)]
pub struct PhiNode {
    pub result: LocalId,
    pub ty: MirTy,
    /// `(predecessor block, operand in that predecessor)`
    pub operands: Vec<(BlockId, Operand)>,
}

// ── Basic blocks ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct BasicBlock {
    pub id: BlockId,
    /// φ-nodes — must appear before all other instructions.
    pub phis: Vec<PhiNode>,
    pub instructions: Vec<Instr>,
    pub terminator: Terminator,
}

impl BasicBlock {
    pub fn new(id: BlockId) -> Self {
        Self {
            id,
            phis: vec![],
            instructions: vec![],
            terminator: Terminator::Unreachable,
        }
    }
}

// ── Functions ─────────────────────────────────────────────────────────────────

/// A single function in the MIR.
///
/// The entry block is always `blocks[0]`.
#[derive(Debug)]
pub struct MirParam {
    pub local: LocalId,
    pub name: Symbol,
    pub ty: MirTy,
}

#[derive(Debug)]
pub struct MirFunction {
    pub id: FunctionId,
    pub name: Symbol,
    pub params: Vec<MirParam>,
    pub return_ty: MirTy,
    pub blocks: Vec<BasicBlock>,
    /// Total number of SSA locals allocated (used to size `locals` array at runtime).
    pub local_count: u32,
}

impl MirFunction {
    pub fn new(id: FunctionId, name: Symbol, return_ty: MirTy) -> Self {
        Self {
            id,
            name,
            params: vec![],
            return_ty,
            blocks: vec![],
            local_count: 0,
        }
    }

    /// Allocate a fresh `LocalId` for this function.
    pub fn alloc_local(&mut self) -> LocalId {
        let id = LocalId(self.local_count);
        self.local_count += 1;
        id
    }

    /// Allocate a fresh `BasicBlock` and return its `BlockId`.
    pub fn alloc_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(BasicBlock::new(id));
        id
    }

    pub fn block(&self, id: BlockId) -> &BasicBlock {
        &self.blocks[id.0 as usize]
    }

    pub fn block_mut(&mut self, id: BlockId) -> &mut BasicBlock {
        &mut self.blocks[id.0 as usize]
    }
}

// ── Object class metadata ─────────────────────────────────────────────────────

/// Object class information embedded in the MIR so the interpreter does not
/// need the HIR at runtime.
#[derive(Debug)]
pub struct MirObjectInfo {
    pub name: Symbol,
    pub parent: Option<Symbol>,
    /// Ordered field names (index == slot index in FidanObject).
    pub field_names: Vec<Symbol>,
    /// Own-method dispatch: `method_sym → FunctionId`.
    pub methods: std::collections::HashMap<Symbol, FunctionId>,
    /// The `initialize` FunctionId (if any).
    pub init_fn: Option<FunctionId>,
}

// ── Program ───────────────────────────────────────────────────────────────────

/// An import declaration propagated from HIR into the MIR program.
///
/// The interpreter uses these at startup to register stdlib namespaces
/// and free-function aliases.
#[derive(Debug, Clone)]
pub struct MirUseDecl {
    /// Module name, e.g. `"io"`, `"math"`.
    pub module: String,
    /// Namespace alias (used when `specific_names` is `None`), e.g. `"io"`.
    pub alias: String,
    /// If `Some`, only these specific function names are imported flat.
    pub specific_names: Option<Vec<String>>,
}

/// The entire program as a collection of MIR functions.
///
/// `functions[0]` is conventionally the top-level initialisation function.
#[derive(Debug, Default)]
pub struct MirProgram {
    pub functions: Vec<MirFunction>,
    /// Object class metadata.  Empty if no objects are defined.
    pub objects: Vec<MirObjectInfo>,
    /// Import declarations from `use std.*` statements.
    pub use_decls: Vec<MirUseDecl>,
}

impl MirProgram {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new function and return its `FunctionId`.
    pub fn add_function(&mut self, f: MirFunction) -> FunctionId {
        let id = FunctionId(self.functions.len() as u32);
        self.functions.push(f);
        id
    }

    pub fn function(&self, id: FunctionId) -> &MirFunction {
        &self.functions[id.0 as usize]
    }

    pub fn function_mut(&mut self, id: FunctionId) -> &mut MirFunction {
        &mut self.functions[id.0 as usize]
    }
}
