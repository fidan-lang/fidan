// fidan-mir/src/display.rs
//
// Human-readable MIR text dump.  Used by `--emit mir`.

use crate::mir::{
    BasicBlock, Callee, Instr, MirLit, MirProgram, MirStringPart, MirTy, Operand, PhiNode, Rvalue,
    Terminator,
};

// ── Public entry point ─────────────────────────────────────────────────────────

pub fn print_program(prog: &MirProgram) {
    for func in &prog.functions {
        println!(
            "fn {}  ->  {}",
            sym_str(func.name.0),
            fmt_ty(&func.return_ty)
        );
        if !func.params.is_empty() {
            let params: Vec<String> = func
                .params
                .iter()
                .map(|p| format!("  param _{}: {}", p.local.0, fmt_ty(&p.ty)))
                .collect();
            for p in params {
                println!("{}", p);
            }
        }
        for bb in &func.blocks {
            print_block(bb);
        }
        println!();
    }
}

// ── Block ──────────────────────────────────────────────────────────────────────

fn print_block(bb: &BasicBlock) {
    println!("  bb{}:", bb.id.0);
    for phi in &bb.phis {
        print_phi(phi);
    }
    for instr in &bb.instructions {
        print_instr(instr);
    }
    print_terminator(&bb.terminator);
}

// ── Φ-nodes ────────────────────────────────────────────────────────────────────

fn print_phi(phi: &PhiNode) {
    let ops: Vec<String> = phi
        .operands
        .iter()
        .map(|(bb, op)| format!("bb{}: {}", bb.0, fmt_op(op)))
        .collect();
    println!(
        "    _{}: {} = φ({})",
        phi.result.0,
        fmt_ty(&phi.ty),
        ops.join(", ")
    );
}

// ── Instructions ───────────────────────────────────────────────────────────────

fn print_instr(instr: &Instr) {
    match instr {
        Instr::Assign { dest, ty, rhs } => {
            println!("    _{}: {} = {}", dest.0, fmt_ty(ty), fmt_rvalue(rhs));
        }
        Instr::Call {
            dest, callee, args, ..
        } => {
            let dest_str = dest.map(|d| format!("_{} = ", d.0)).unwrap_or_default();
            let args_str = args.iter().map(fmt_op).collect::<Vec<_>>().join(", ");
            println!("    {}{}({})", dest_str, fmt_callee(callee), args_str);
        }
        Instr::SetField {
            object,
            field,
            value,
        } => {
            println!(
                "    {}.{} = {}",
                fmt_op(object),
                sym_str(field.0),
                fmt_op(value)
            );
        }
        Instr::GetField {
            dest,
            object,
            field,
        } => {
            println!("    _{} = {}.{}", dest.0, fmt_op(object), sym_str(field.0));
        }
        Instr::GetIndex {
            dest,
            object,
            index,
        } => {
            println!("    _{} = {}[{}]", dest.0, fmt_op(object), fmt_op(index));
        }
        Instr::SetIndex {
            object,
            index,
            value,
        } => {
            println!(
                "    {}[{}] = {}",
                fmt_op(object),
                fmt_op(index),
                fmt_op(value)
            );
        }
        Instr::Drop { local } => {
            println!("    drop _{}", local.0);
        }
        Instr::AwaitPending { dest, handle } => {
            println!("    _{} = await {}", dest.0, fmt_op(handle));
        }
        Instr::SpawnConcurrent {
            handle,
            task_fn,
            args,
        } => {
            let args_str = args.iter().map(fmt_op).collect::<Vec<_>>().join(", ");
            println!(
                "    _{} = spawn_concurrent fn{}({})",
                handle.0, task_fn.0, args_str
            );
        }
        Instr::SpawnParallel {
            handle,
            task_fn,
            args,
        } => {
            let args_str = args.iter().map(fmt_op).collect::<Vec<_>>().join(", ");
            println!(
                "    _{} = spawn_parallel fn{}({})",
                handle.0, task_fn.0, args_str
            );
        }
        Instr::JoinAll { handles } => {
            let hs = handles
                .iter()
                .map(|h| format!("_{}", h.0))
                .collect::<Vec<_>>()
                .join(", ");
            println!("    join_all({})", hs);
        }
        Instr::SpawnExpr {
            dest,
            task_fn,
            args,
        } => {
            let args_str = args.iter().map(fmt_op).collect::<Vec<_>>().join(", ");
            println!("    _{} = spawn fn{}({})", dest.0, task_fn.0, args_str);
        }
        Instr::SpawnDynamic { dest, method, args } => {
            let args_str = args.iter().map(fmt_op).collect::<Vec<_>>().join(", ");
            match method {
                Some(sym) => println!("    _{} = spawn_method .{}({})", dest.0, sym.0, args_str),
                None => println!("    _{} = spawn_dynamic ({})", dest.0, args_str),
            }
        }
        Instr::ParallelIter {
            collection,
            body_fn,
            closure_args,
        } => {
            let ca = closure_args
                .iter()
                .map(fmt_op)
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "    parallel_iter {} fn{}[{}]",
                fmt_op(collection),
                body_fn.0,
                ca
            );
        }
        Instr::Nop => {
            println!("    nop");
        }
        Instr::PushCatch(bb) => {
            println!("    push_catch bb{}", bb.0);
        }
        Instr::PopCatch => {
            println!("    pop_catch");
        }
        Instr::CertainCheck { operand, name } => {
            println!(
                "    certain_check {} ({})",
                fmt_op(operand),
                sym_str(name.0)
            );
        }
        Instr::LoadGlobal { dest, global } => {
            println!("    _{}: dyn = load_global g{}", dest.0, global.0);
        }
        Instr::StoreGlobal { global, value } => {
            println!("    store_global g{} = {}", global.0, fmt_op(value));
        }
    }
}

// ── Terminator ─────────────────────────────────────────────────────────────────

fn print_terminator(term: &Terminator) {
    match term {
        Terminator::Return(None) => println!("    return"),
        Terminator::Return(Some(op)) => println!("    return {}", fmt_op(op)),
        Terminator::Goto(bb) => println!("    goto bb{}", bb.0),
        Terminator::Branch {
            cond,
            then_bb,
            else_bb,
        } => {
            println!(
                "    if {} {{ goto bb{} }} else {{ goto bb{} }}",
                fmt_op(cond),
                then_bb.0,
                else_bb.0
            );
        }
        Terminator::Throw { value } => println!("    throw {}", fmt_op(value)),
        Terminator::Unreachable => println!("    unreachable"),
    }
}

// ── Formatting helpers ─────────────────────────────────────────────────────────

fn fmt_ty(ty: &MirTy) -> String {
    match ty {
        MirTy::Integer => "int".to_string(),
        MirTy::Float => "float".to_string(),
        MirTy::Boolean => "bool".to_string(),
        MirTy::String => "string".to_string(),
        MirTy::Nothing => "nothing".to_string(),
        MirTy::Dynamic => "dynamic".to_string(),
        MirTy::List(e) => format!("list<{}>", fmt_ty(e)),
        MirTy::Dict(k, v) => format!("dict<{}, {}>", fmt_ty(k), fmt_ty(v)),
        MirTy::Tuple(ts) => format!("({})", ts.iter().map(fmt_ty).collect::<Vec<_>>().join(", ")),
        MirTy::Object(s) => sym_str(s.0),
        MirTy::Enum(s) => format!("enum({})", sym_str(s.0)),
        MirTy::Shared(t) => format!("shared<{}>", fmt_ty(t)),
        MirTy::Pending(t) => format!("pending<{}>", fmt_ty(t)),
        MirTy::Function => "action".to_string(),
        MirTy::Error => "<error>".to_string(),
    }
}

fn fmt_op(op: &Operand) -> String {
    match op {
        Operand::Local(l) => format!("_{}", l.0),
        Operand::Const(MirLit::Int(i)) => i.to_string(),
        Operand::Const(MirLit::Float(f)) => format!("{:.6}", f),
        Operand::Const(MirLit::Bool(b)) => b.to_string(),
        Operand::Const(MirLit::Str(s)) => format!("{:?}", s),
        Operand::Const(MirLit::Nothing) => "nothing".to_string(),
        Operand::Const(MirLit::FunctionRef(id)) => format!("fn#{}", id),
        Operand::Const(MirLit::Namespace(m)) => format!("std.{}", m),
        Operand::Const(MirLit::StdlibFn { module, name }) => format!("std.{}.{}", module, name),
        Operand::Const(MirLit::EnumType(e)) => format!("enum:{}", e),
        Operand::Const(MirLit::ClassType(c)) => format!("class:{}", c),
    }
}

fn fmt_callee(callee: &Callee) -> String {
    match callee {
        Callee::Fn(id) => format!("fn{}", id.0),
        Callee::Method { receiver, method } => {
            format!("{}.{}", fmt_op(receiver), sym_str(method.0))
        }
        Callee::Builtin(sym) => format!("builtin({})", sym_str(sym.0)),
        Callee::Dynamic(op) => format!("dyn({})", fmt_op(op)),
    }
}

fn fmt_rvalue(rv: &Rvalue) -> String {
    match rv {
        Rvalue::Use(op) => fmt_op(op),
        Rvalue::Binary { op, lhs, rhs } => {
            format!("{} {:?} {}", fmt_op(lhs), op, fmt_op(rhs))
        }
        Rvalue::Unary { op, operand } => format!("{:?} {}", op, fmt_op(operand)),
        Rvalue::NullCoalesce { lhs, rhs } => format!("{} ?? {}", fmt_op(lhs), fmt_op(rhs)),
        Rvalue::Call { callee, args } => {
            let a = args.iter().map(fmt_op).collect::<Vec<_>>().join(", ");
            format!("{}({})", fmt_callee(callee), a)
        }
        Rvalue::Construct { ty, fields } => {
            let fs: Vec<String> = fields
                .iter()
                .map(|(k, v)| format!("{}: {}", sym_str(k.0), fmt_op(v)))
                .collect();
            format!("{}{{ {} }}", sym_str(ty.0), fs.join(", "))
        }
        Rvalue::List(elems) => {
            format!(
                "[{}]",
                elems.iter().map(fmt_op).collect::<Vec<_>>().join(", ")
            )
        }
        Rvalue::Dict(pairs) => {
            let ps: Vec<String> = pairs
                .iter()
                .map(|(k, v)| format!("{}: {}", fmt_op(k), fmt_op(v)))
                .collect();
            format!("{{{}}}", ps.join(", "))
        }
        Rvalue::Tuple(elems) => {
            format!(
                "({})",
                elems.iter().map(fmt_op).collect::<Vec<_>>().join(", ")
            )
        }
        Rvalue::StringInterp(parts) => {
            let s: String = parts
                .iter()
                .map(|p| match p {
                    MirStringPart::Literal(s) => s.clone(),
                    MirStringPart::Operand(op) => format!("{{{}}}", fmt_op(op)),
                })
                .collect();
            format!("\"{}\"", s)
        }
        Rvalue::Literal(lit) => fmt_op(&Operand::Const(lit.clone())),
        Rvalue::CatchException => "catch_exception".to_string(),
        Rvalue::Slice { target, start, end, inclusive, step } => {
            let op = if *inclusive { "..." } else { ".." };
            let s  = start.as_ref().map(fmt_op).unwrap_or_default();
            let e  = end.as_ref().map(fmt_op).unwrap_or_default();
            let st = step.as_ref().map(|x| format!(" step {}", fmt_op(x))).unwrap_or_default();
            format!("{}[{}{}{}{}]", fmt_op(target), s, op, e, st)
        }
    }
}

/// Return a human-readable placeholder for a `Symbol`.
/// Symbols are interned integers — we can only show the raw ID without the
/// interner.  Codegen passes that need the real string have access to it.
fn sym_str(id: u32) -> String {
    format!("sym#{}", id)
}
