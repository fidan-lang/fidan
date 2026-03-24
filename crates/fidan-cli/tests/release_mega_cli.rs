use std::sync::Arc;

use fidan_lexer::{Lexer, SymbolInterner};
use fidan_source::{FileId, SourceFile, SourceMap};

fn lower_release_mega() -> (Arc<SymbolInterner>, fidan_mir::MirProgram) {
    let interner = Arc::new(SymbolInterner::new());
    let source = include_str!("../../../test/examples/release_mega_1_0.fdn");
    let file = SourceFile::new(FileId(0), "<release-mega>", source);
    let (tokens, _) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
    let (module, _) = fidan_parser::parse(&tokens, FileId(0), Arc::clone(&interner));
    let typed = fidan_typeck::typecheck_full(&module, Arc::clone(&interner));
    let hir = fidan_hir::lower_module(&module, &typed, &interner);
    let mut mir = fidan_mir::lower_program(&hir, &interner, &[]);
    fidan_passes::run_all(&mut mir);
    (interner, mir)
}

#[test]
fn optimized_release_mega_keeps_entry_blocks() {
    let (interner, mir) = lower_release_mega();

    let blockless: Vec<String> = mir
        .functions
        .iter()
        .filter(|func| func.blocks.is_empty())
        .map(|func| interner.resolve(func.name).to_string())
        .collect();

    assert!(
        blockless.is_empty(),
        "functions without entry block after optimization: {blockless:?}"
    );
}

#[test]
fn release_mega_test_mode_runs_without_internal_crash() {
    let (interner, mir) = lower_release_mega();
    let test_fns = mir.test_functions.clone();

    let source_map = Arc::new(SourceMap::default());
    let (result, results) = fidan_interp::run_tests(mir, interner, source_map);

    if let Err(err) = result {
        panic!("test runner failed: {} {}", err.code, err.message);
    }
    let failed: Vec<(String, Option<String>)> = results
        .into_iter()
        .filter(|case| !case.passed)
        .map(|case| (case.name, case.message))
        .collect();
    assert!(
        failed.is_empty(),
        "release mega tests failed unexpectedly: {failed:?}; total tests: {}",
        test_fns.len()
    );
}
