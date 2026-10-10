//! Tests of the `aetheryx` command line. Expected outputs are what `nix`
//! (Nix 2.28) prints for the same command.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Runs `aetheryx args` in `dir`; returns (exit code, stdout, stderr).
fn run_in(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_aetheryx"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("aetheryx runs");
    (
        output.status.code().expect("aetheryx exits"),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

fn run(args: &[&str]) -> (i32, String, String) {
    run_in(&std::env::temp_dir(), args)
}

/// Runs a command that must succeed; returns its standard output.
fn stdout(args: &[&str]) -> String {
    let (code, out, err) = run(args);
    assert_eq!(code, 0, "{args:?}: {err}");
    out
}

/// A fresh directory with the given files.
fn dir_with(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aetheryx-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

#[test]
fn values_print_like_nix_eval() {
    assert_eq!(stdout(&["eval", "--expr", "1+2"]), "3\n");
    assert_eq!(
        stdout(&["eval", "--expr", "x: x"]),
        "«lambda @ «string»:1:1»\n"
    );
    assert_eq!(
        stdout(&[
            "eval",
            "--expr",
            "{ f = { a }: a; g = map; h = builtins.add 1; }"
        ]),
        "{ f = «lambda f @ «string»:1:7»; g = «primop map»; h = «partially applied primop add»; }\n"
    );
    assert_eq!(
        stdout(&["eval", "--expr", "let a = [ 1 ]; in [ a a ]"]),
        "[ [ 1 ] «repeated» ]\n"
    );
    assert_eq!(stdout(&["eval", "--expr", "__curPos"]), "null\n");
}

#[test]
fn errors_inside_values_are_printed_in_place() {
    assert_eq!(
        stdout(&[
            "eval",
            "--expr",
            r#"[ (1 + "x") (throw "a\nb") (let x = x; in x) (abort "z") ({}.q) ]"#
        ]),
        "[ «error: cannot add a string to an integer» «error: a\nb» \
         «error: infinite recursion encountered» \
         «error: evaluation aborted with the following error message: 'z'» \
         «error: attribute 'q' missing» ]\n"
    );
    // An error in the value itself fails the command.
    let (code, out, err) = run(&["eval", "--expr", r#"throw "top""#]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(err.contains("top"), "{err}");
}

#[test]
fn json_and_raw_output() {
    assert_eq!(
        stdout(&[
            "eval",
            "--json",
            "--expr",
            r#"{ b = 1; a = "x"; c = { d = null; }; }"#
        ]),
        "{\"a\":\"x\",\"b\":1,\"c\":{\"d\":null}}\n"
    );
    assert_eq!(
        stdout(&[
            "eval",
            "--json",
            "--expr",
            r#"{ __toString = s: "str"; a = 1; }"#
        ]),
        "\"str\"\n"
    );
    assert_eq!(
        stdout(&["eval", "--raw", "--expr", r#"{ outPath = "/x"; }"#]),
        "/x"
    );
    let (code, _, err) = run(&["eval", "--json", "--expr", "map"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("cannot convert the built-in function 'map' to JSON"),
        "{err}"
    );
    let (code, _, err) = run(&["eval", "--raw", "--expr", "5"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("cannot coerce an integer to a string: 5"),
        "{err}"
    );
}

#[test]
fn files_attribute_paths_and_arguments() {
    let dir = dir_with(
        "files",
        &[
            ("fn.nix", "{ x ? 1, y }: { sum = x + y; inherit x y; }"),
            ("set.nix", "{ a = { b = [ 10 20 ]; }; }"),
            ("pkg/default.nix", "42"),
        ],
    );
    let fn_nix = dir.join("fn.nix");
    let ok = |args: &[&str]| {
        let (code, out, err) = run_in(&dir, args);
        assert_eq!(code, 0, "{args:?}: {err}");
        out
    };
    assert_eq!(ok(&["eval", "-f", "set.nix", "a.b.1"]), "20\n");
    assert_eq!(ok(&["eval", "-f", "pkg"]), "42\n");
    // Without an attribute path, the function is not called.
    assert_eq!(
        ok(&["eval", "-f", "fn.nix", "--arg", "y", "2"]),
        format!("«lambda @ {}:1:1»\n", fn_nix.display())
    );
    assert_eq!(
        ok(&["eval", "-f", "fn.nix", "sum", "--arg", "y", "2"]),
        "3\n"
    );
    assert_eq!(
        ok(&["eval", "-f", "fn.nix", "y", "--argstr", "y", "s"]),
        "\"s\"\n"
    );
    assert_eq!(
        ok(&["eval", "--expr", "[ 1 2 3 ]", "--apply", "builtins.length"]),
        "3\n"
    );
    for (args, message) in [
        (
            &["eval", "--expr", "{ a = 1; }", "b"][..],
            "attribute 'b' in selection path 'b' not found",
        ),
        (
            &["eval", "--expr", "{ a = 1; }", "b"][..],
            "Did you mean a?",
        ),
        (
            &["eval", "-f", "set.nix", "a.b.5"][..],
            "list index 5 in selection path 'a.b.5' is out of range",
        ),
        (
            &["eval", "-f", "fn.nix", "sum"][..],
            "cannot evaluate a function that has an argument without a value ('y')",
        ),
    ] {
        let (code, _, err) = run_in(&dir, args);
        assert_eq!(code, 1, "{args:?}");
        assert!(err.contains(message), "{args:?}: {err}");
    }
    let (code, _, err) = run_in(&dir, &["eval", "-f", "missing.nix"]);
    assert_eq!(code, 1);
    assert!(
        err.contains(&format!(
            "path '{}' does not exist",
            dir.join("missing.nix").display()
        )),
        "{err}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn usage_errors_are_reported_like_nix() {
    for (args, message) in [
        (
            &["eval", "--bogus"][..],
            "error: unrecognised flag '--bogus'\n",
        ),
        (
            &["eval", "--file", "a.nix", "--expr", "1"][..],
            "error: '--file' and '--expr' are exclusive\n",
        ),
        (
            &["eval", "--json", "--raw", "--expr", "1"][..],
            "error: --raw and --json are mutually exclusive\n",
        ),
        (
            &["foo"][..],
            "error: 'foo' is not a recognised command\n       Did you mean one of fmt or log?\n",
        ),
    ] {
        let (code, out, err) = run(args);
        assert_eq!((code, out.as_str()), (1, ""), "{args:?}");
        assert!(err.starts_with(message), "{args:?}: {err}");
    }
}

#[test]
fn unsupported_commands_fail_explicitly() {
    for (args, message) in [
        (
            &["build", ".#foo"][..],
            "'nix build' is not supported by aetheryx yet",
        ),
        (
            &["develop"][..],
            "'nix develop' is not supported by aetheryx yet",
        ),
        (
            &["eval", "nixpkgs#hello"][..],
            "evaluating flakes ('nixpkgs#hello') is not supported by aetheryx yet",
        ),
        (
            &["eval", "-I", "x=y", "--expr", "1"][..],
            "the search path (-I) is not supported",
        ),
        (
            &["--option", "pure-eval", "true", "eval", "--expr", "1"][..],
            "the setting 'pure-eval' is not supported by aetheryx yet",
        ),
    ] {
        let (code, out, err) = run(args);
        assert_eq!((code, out.as_str()), (1, ""), "{args:?}");
        assert!(err.contains(message), "{args:?}: {err}");
    }
}

#[test]
fn version_and_global_options() {
    assert_eq!(
        stdout(&["--version"]),
        format!("aetheryx {}\n", env!("CARGO_PKG_VERSION"))
    );
    // Logging and experimental-feature options are accepted anywhere.
    assert_eq!(
        stdout(&[
            "-L",
            "eval",
            "--extra-experimental-features",
            "nix-command",
            "--expr",
            "1",
            "-v"
        ]),
        "1\n"
    );
}
