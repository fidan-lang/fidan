use super::*;

#[test]
fn diagnostic_identity_survives_actual_native_jit_calls() {
    let cases = [
        ("", "panic(\"error [R2001]: user text\")", "R1002", false),
        (
            "",
            "var zero oftype flexible = 0; print(1 / zero)",
            "R2001",
            false,
        ),
        (
            "",
            "var values = [1]; var index = 2; print(values[index])",
            "R2002",
            false,
        ),
        (
            "",
            "var value oftype flexible = 9223372036854775807; print(value + 1)",
            "R2003",
            false,
        ),
        (
            "",
            "var value oftype flexible = \"bad\"; print(value - 1)",
            "R0001",
            false,
        ),
        ("use std.io", "io.readFile(\"\")", "R3001", false),
        ("use std.json", "json.loads(\"invalid\")", "R3005", false),
        (
            "action descend with (certain n oftype integer) returns integer { if n == 0 { return 0 }; return descend(n - 1) }",
            "descend(4096)",
            "R1001",
            false,
        ),
        (
            "",
            "parallel { task fail { panic(\"task failed\") } }",
            "R9001",
            false,
        ),
        ("use std.io", "io.readFile(\"\")", "R4001", true),
        (
            "use std.io",
            "io.writeFile(\"never-created\", \"denied\")",
            "R4002",
            true,
        ),
        (
            "use std.io",
            "io.getEnv(\"FIDAN_DIAGNOSTIC_TEST_SECRET\")",
            "R4003",
            true,
        ),
    ];
    for (setup, body, expected, sandbox) in cases {
        let source = format!(
            "{setup}\naction failure {{ {body} }}\naction diagnostic_probe {{ failure() }}\ndiagnostic_probe()"
        );
        let interner = Arc::new(SymbolInterner::new());
        let sources = Arc::new(SourceMap::new());
        let file = sources.add_file("<native-diagnostic-bridge>", source);
        let (tokens, diagnostics) =
            fidan_lexer::Lexer::new(&file, Arc::clone(&interner)).tokenise();
        assert!(diagnostics.is_empty());
        let (module, diagnostics) = fidan_parser::parse(&tokens, file.id, Arc::clone(&interner));
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let typed = fidan_typeck::typecheck_full(&module, Arc::clone(&interner));
        assert!(
            !typed
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == fidan_diagnostics::Severity::Error),
            "{:?}",
            typed.diagnostics
        );
        let hir = fidan_hir::lower_module(&module, &typed, &interner);
        let mut program = fidan_mir::lower_program(&hir, &interner, &[]);
        fidan_passes::run_preserving_call_frames(&mut program);
        let mut machine = MirMachine::new(Arc::new(program), interner, sources);
        machine.set_jit_threshold(1);
        if sandbox {
            machine.set_sandbox(fidan_stdlib::SandboxPolicy::default());
        }
        let error = machine.run().expect_err("diagnostic program must fail");
        assert_eq!(error.code.0, expected, "{}", error.message);
        let probe = machine
            .program
            .functions
            .iter()
            .find(|function| machine.interner.resolve(function.name).as_ref() == "diagnostic_probe")
            .unwrap();
        assert!(
            machine.jit_fns.read()[probe.id.0 as usize]
                .as_ref()
                .is_some_and(JitFnEntry::is_native),
            "{expected} must cross a real native JIT call"
        );
    }
}
