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
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("TARGET").as_deref() != Ok("aarch64-apple-darwin") {
        return;
    }
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let root = Path::new(&manifest).join("../..");
    let source_dir = root.join("src/macos");
    let mut sources: Vec<_> = fs::read_dir(&source_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "swift"))
        .collect();
    sources.sort();
    println!("cargo:rerun-if-changed={}", source_dir.display());
    for source in &sources {
        println!("cargo:rerun-if-changed={}", source.display());
    }
    // Tests execute the same UI source as release packages, always in headless mode.
    let output = root.join("bin/cito-ui-darwin-arm64");
    let temporary = output.with_extension("tmp");
    let mut args = vec!["-O", "-target", "arm64-apple-macos13.0"];
    args.extend(sources.iter().map(|source| source.to_str().unwrap()));
    args.extend(["-o", temporary.to_str().unwrap()]);
    run("/usr/bin/swiftc", &args);
    run("/usr/bin/strip", &["-x", temporary.to_str().unwrap()]);
    run(
        "/usr/bin/codesign",
        &["--force", "--sign", "-", temporary.to_str().unwrap()],
    );
    fs::rename(&temporary, &output).unwrap();
}
