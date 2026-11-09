use std::{env, fs, path::Path, process::Command};

fn run(program: &str, args: &[&str]) {
    let status = Command::new(program)
        .args(args)
        .status()
        .unwrap_or_else(|e| {
            panic!("Cannot run {program}: {e}");
        });
    assert!(status.success(), "{program} failed: {status}");
}

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let root = Path::new(&manifest).join("../..");
    let source = root.join("src/macos/cito-ui.swift");
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-changed=build.rs");
    // Tests execute the same UI source as release packages, always in headless mode.
    if env::var("TARGET").as_deref() != Ok("aarch64-apple-darwin") {
        return;
    }
    let output = root.join("bin/cito-ui-darwin-arm64");
    let staged = root.join("plugins/human/bin/cito-ui");
    let temporary = output.with_extension("tmp");
    run(
        "/usr/bin/swiftc",
        &[
            "-O",
            "-target",
            "arm64-apple-macos13.0",
            source.to_str().unwrap(),
            "-o",
            temporary.to_str().unwrap(),
        ],
    );
    run("/usr/bin/strip", &["-x", temporary.to_str().unwrap()]);
    run(
        "/usr/bin/codesign",
        &["--force", "--sign", "-", temporary.to_str().unwrap()],
    );
    fs::rename(&temporary, &output).unwrap();
    fs::create_dir_all(staged.parent().unwrap()).unwrap();
    fs::copy(output, staged).unwrap();
}
