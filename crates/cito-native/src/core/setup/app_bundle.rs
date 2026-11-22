//! Application bundle assembly with symlink-preserving resource copying.
use super::{
    copy_dir_recursive, ensure_cli_shims_native, publish_directory, resolve_app_bundle_path,
    resolve_bundled_plugins_root, resolve_package_root, Candidate,
};
use crate::plugins::{
    is_staged_plugins_root, resolve_builtin_plugins_dir, LoadedPlugins, CITO_VERSION,
};
use std::os::unix::fs::PermissionsExt;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

// Finalize the private candidate, including resources created by codesign.
// Never follow a copied symlink when changing permissions.
fn finalize_bundle(path: &Path) -> std::io::Result<u64> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Ok(meta.len());
    }
    let executable = meta.is_dir() || meta.permissions().mode() & 0o111 != 0;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
    )?;
    if !meta.is_dir() {
        return Ok(meta.len());
    }
    fs::read_dir(path)?.try_fold(0, |bytes, entry| {
        Ok(bytes + finalize_bundle(&entry?.path())?)
    })
}

pub fn build_mac_app_bundle_native(
    dest_dir: Option<&str>,
    state_dir: &Path,
    plugins: &LoadedPlugins,
) -> Result<(PathBuf, u64), String> {
    build_mac_app_bundle_from_root(dest_dir, state_dir, plugins, &resolve_package_root())
}

pub(super) fn build_mac_app_bundle_from_root(
    dest_dir: Option<&str>,
    state_dir: &Path,
    plugins: &LoadedPlugins,
    pkg_root: &Path,
) -> Result<(PathBuf, u64), String> {
    let destination = resolve_app_bundle_path(dest_dir, state_dir)?;
    let app_path =
        destination.with_file_name(format!(".Cito-{}.app", crate::plugins::random_hex(8)));
    let _candidate = Candidate(app_path.clone());
    let contents_dir = app_path.join("Contents");
    let macos_dir = contents_dir.join("MacOS");
    let helpers_dir = contents_dir.join("Helpers");
    let resources_dir = contents_dir.join("Resources");

    let _ = fs::remove_dir_all(&app_path);
    fs::create_dir_all(&macos_dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(&helpers_dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(&resources_dir).map_err(|e| e.to_string())?;

    let info_plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
           <key>CFBundleIdentifier</key>\n\
           <string>one.tefx.cito</string>\n\
           <key>CFBundleName</key>\n\
           <string>Cito</string>\n\
           <key>CFBundleDisplayName</key>\n\
           <string>Cito</string>\n\
           <key>CFBundleExecutable</key>\n\
           <string>Cito</string>\n\
           <key>CFBundleIconFile</key>\n\
           <string>AppIcon</string>\n\
           <key>CFBundleIconName</key>\n\
           <string>AppIcon</string>\n\
           <key>CFBundleShortVersionString</key>\n\
           <string>{CITO_VERSION}</string>\n\
           <key>CFBundleVersion</key>\n\
           <string>{CITO_VERSION}</string>\n\
           <key>CFBundlePackageType</key>\n\
           <string>APPL</string>\n\
           <key>LSUIElement</key>\n\
           <true/>\n\
           <key>LSMinimumSystemVersion</key>\n\
           <string>13.0</string>\n\
         </dict>\n\
         </plist>\n"
    );
    fs::write(contents_dir.join("Info.plist"), info_plist).map_err(|e| e.to_string())?;

    let launcher = r#"#!/bin/sh
set -e
INVOKED_AS="$(basename "$0")"
SOURCE="$0"
while [ -L "$SOURCE" ]; do
  DIR="$(cd -P "$(dirname "$SOURCE")" && pwd)"
  TARGET="$(readlink "$SOURCE")"
  case "$TARGET" in
    /*) SOURCE="$TARGET" ;;
    *) SOURCE="$DIR/$TARGET" ;;
  esac
done
APP_CONTENTS="$(cd -P "$(dirname "$SOURCE")/.." && pwd)"
export CITO_BUILTIN_PLUGINS_DIR="${CITO_BUILTIN_PLUGINS_DIR:-$APP_CONTENTS/Resources/plugins}"
if [ "$INVOKED_AS" = "cito-mcp" ]; then
  export CITO_ENDPOINT_PID="${CITO_ENDPOINT_PID:-$PPID}"
  if [ "$#" -eq 0 ]; then
    exec "$APP_CONTENTS/Helpers/cito-native" mcp-stdio
  fi
  exec "$APP_CONTENTS/Helpers/cito-native" "$@"
fi
if [ "$#" -eq 0 ]; then
  exec "$APP_CONTENTS/Helpers/cito-native" daemon
fi
case "$1" in
  -psn_*)
    shift
    exec "$APP_CONTENTS/Helpers/cito-native" daemon "$@"
    ;;
esac
exec "$APP_CONTENTS/Helpers/cito-native" "$@"
"#;
    let launcher_path = macos_dir.join("Cito");
    fs::write(&launcher_path, launcher).map_err(|e| e.to_string())?;
    fs::set_permissions(&launcher_path, fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())?;

    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let helper_native = helpers_dir.join("cito-native");
    fs::copy(&exe, &helper_native).map_err(|e| e.to_string())?;
    fs::set_permissions(&helper_native, fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())?;

    for ui_cand in [
        pkg_root.join("bin/cito-ui-darwin-arm64"),
        pkg_root.join("Helpers/cito-ui"),
    ] {
        if ui_cand.is_file() {
            let helper_ui = helpers_dir.join("cito-ui");
            fs::copy(&ui_cand, &helper_ui).map_err(|e| e.to_string())?;
            fs::set_permissions(&helper_ui, fs::Permissions::from_mode(0o755))
                .map_err(|e| e.to_string())?;
            break;
        }
    }

    if !helpers_dir.join("cito-ui").is_file() {
        return Err("App bundle requires cito-ui".into());
    }

    for icon_cand in [
        pkg_root.join("bin/AppIcon.icns"),
        pkg_root.join("Resources/AppIcon.icns"),
    ] {
        if icon_cand.is_file() {
            fs::copy(&icon_cand, resources_dir.join("AppIcon.icns")).map_err(|e| e.to_string())?;
            break;
        }
    }

    if !resources_dir.join("AppIcon.icns").is_file() {
        return Err("App bundle requires AppIcon.icns".into());
    }

    if let Some(mut plugins_src) = resolve_builtin_plugins_dir() {
        if is_staged_plugins_root(&plugins_src, state_dir) {
            // Runtime staging may omit overridden IDs; keep their bundled fallback in the App.
            plugins_src = resolve_bundled_plugins_root().unwrap_or(plugins_src);
        }
        copy_dir_recursive(&plugins_src, &resources_dir.join("plugins"))?;
    }

    ensure_cli_shims_native(state_dir, plugins)?;
    let _ = Command::new("/usr/bin/xattr")
        .args(["-cr", &app_path.to_string_lossy()])
        .status();

    let sign_id = env::var("CITO_CODESIGN_IDENTITY")
        .or_else(|_| env::var("APPLE_SIGNING_IDENTITY"))
        .unwrap_or_default();
    let status = if !sign_id.trim().is_empty() {
        Command::new("/usr/bin/codesign")
            .args([
                "--force",
                "--options",
                "runtime",
                "--timestamp",
                "--deep",
                "--sign",
                sign_id.trim(),
                &app_path.to_string_lossy(),
            ])
            .status()
    } else {
        Command::new("/usr/bin/codesign")
            .args(["-s", "-", "--force", "--deep", &app_path.to_string_lossy()])
            .status()
    }
    .map_err(|e| format!("Cannot run codesign for {}: {e}", app_path.display()))?;
    if !status.success() {
        return Err(format!(
            "codesign failed for {}: {status}",
            app_path.display()
        ));
    }

    let bytes = finalize_bundle(&app_path).map_err(|e| e.to_string())?;
    publish_directory(&app_path, &destination)?;
    // The candidate guard removes the exchanged previous generation non-fatally.
    Ok((destination, bytes))
}
