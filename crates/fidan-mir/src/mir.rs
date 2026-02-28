// fidan-mir stubs — Phase 5
use fidan_lexer::Symbol;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub struct BlockId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub struct LocalId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub struct FunctionId(pub u32);

#[derive(Debug, Clone)] pub enum Operand { Local(LocalId), Const(ConstValue) }
#[derive(Debug, Clone)] pub enum ConstValue { Int(i64), Float(f64), Bool(bool), Nothing }
#[derive(Debug)] pub struct BasicBlock { pub id: BlockId, pub instructions: Vec<Instruction>, pub terminator: Terminator }
#[derive(Debug)] pub enum Instruction { Nop }
#[derive(Debug)] pub enum Terminator { Return(Option<Operand>), Jump(BlockId), CondJump { cond: Operand, then_bb: BlockId, else_bb: BlockId } }
#[derive(Debug)] pub struct MirFunction { pub id: FunctionId, pub name: Symbol, pub blocks: Vec<BasicBlock> }
#[derive(Debug, Default)] pub struct MirProgram { pub functions: Vec<MirFunction> }
