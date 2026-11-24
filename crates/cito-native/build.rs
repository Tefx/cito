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
    let output = root.join("bin/cito-ui-darwin-arm64");
    let modified = |path: &Path| {
        fs::metadata(path)
            .ok()
            .filter(|m| m.is_file())
            .and_then(|m| m.modified().ok())
    };
    let release = env::var("PROFILE").as_deref() == Ok("release");
    if !release
        && modified(&output).is_some_and(|built| {
            sources
                .iter()
                .chain([&Path::new(&manifest).join("build.rs")])
                .all(|input| modified(input).is_some_and(|changed| changed <= built))
        })
    {
        return;
    }
    // A checkout does not preserve commit timestamps. Rebuild stale inputs for the
    // test runner without publishing a path-dependent binary into the worktree.
    let output = if release {
        output
    } else {
        let private = Path::new(&env::var_os("OUT_DIR").unwrap()).join("cito-ui");
        println!("cargo:rustc-env=CITO_TEST_BUILT_UI={}", private.display());
        private
    };
    let temporary = output.with_extension(format!("{}.tmp", std::process::id()));
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
