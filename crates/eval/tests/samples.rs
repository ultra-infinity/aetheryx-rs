//! The oracle test: every sample in `samples/valid` must evaluate to exactly
//! what real Nix printed for it (`samples/expected`, from
//! `samples/gen-expected.sh`).

use std::path::{Path, PathBuf};

use eval::evaluate;
use ir::ast_to_hir::LowerOptions;
use ir::diagnostics::SourceFile;
use pliron::context::Context;

/// Samples that use features not supported yet, with the reason.
const UNSUPPORTED: &[(&str, &str)] = &[(
    "13-search-path",
    "search paths (`<name>`) need `builtins.findFile` and a search path",
)];

/// The repository root and home directory the samples are evaluated with;
/// they replace `@ROOT@` and `@HOME@` in the expected outputs.
const ROOT: &str = "/repo";
const HOME: &str = "/home/u";

fn samples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples")
}

fn evaluate_sample(name: &str) -> Result<String, String> {
    let text = std::fs::read_to_string(samples_dir().join(format!("valid/{name}.nix"))).unwrap();
    let ctx = Context::new();
    let path = format!("samples/valid/{name}.nix");
    let file = SourceFile::with_cwd(&ctx, path, text, Path::new(ROOT)).unwrap();
    let options = LowerOptions {
        home_dir: Some(HOME.to_string()),
    };
    evaluate(ctx, &file, &options).map_err(|e| e.to_string())
}

fn expected(name: &str) -> String {
    let path = samples_dir().join(format!("expected/{name}.out"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}; run samples/gen-expected.sh", path.display()))
        .trim_end()
        .replace("@ROOT@", ROOT)
        .replace("@HOME@", HOME)
}

fn sample_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(samples_dir().join("valid"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "nix"))
        .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(!names.is_empty());
    names
}

#[test]
fn samples_evaluate_like_nix() {
    let mut failures = Vec::new();
    let mut compared = 0;
    for name in sample_names() {
        if UNSUPPORTED.iter().any(|(n, _)| *n == name) {
            continue;
        }
        compared += 1;
        match evaluate_sample(&name) {
            Ok(output) if output == expected(&name) => {}
            Ok(output) => failures.push(format!(
                "{name}:\n  ours: {output}\n  nix:  {}",
                expected(&name)
            )),
            Err(error) => failures.push(format!("{name}: error: {error}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(compared, sample_names().len() - UNSUPPORTED.len());
}

#[test]
fn unsupported_samples_fail_clearly() {
    for (name, reason) in UNSUPPORTED {
        let error = evaluate_sample(name).expect_err(name);
        assert!(
            error.contains("not supported"),
            "{name} ({reason}): {error}"
        );
    }
}

#[test]
fn every_sample_has_an_expected_output() {
    for name in sample_names() {
        assert!(
            samples_dir().join(format!("expected/{name}.out")).exists(),
            "{name}: run samples/gen-expected.sh"
        );
    }
}
