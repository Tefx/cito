#!/usr/bin/env node
// Build-time only: install every native target before discarding reproducible Cargo output.
import { spawnSync } from "node:child_process";
import { chmod, copyFile, mkdir, readFile, readdir, rm } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const manifest = join(root, "crates/cito-native/Cargo.toml");
const targetDir = join(root, "crates/cito-native/target");
const platform = `${process.platform}-${process.arch}`;
if (platform !== "darwin-arm64") throw new Error(`The packaged macOS application requires darwin-arm64; received ${platform}`);

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { cwd: root, stdio: "inherit", ...options });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} exited ${result.status ?? result.signal}`);
  return result;
}
async function install(source, destination) {
  await mkdir(dirname(destination), { recursive: true });
  await copyFile(source, destination);
  await chmod(destination, 0o755);
  run("strip", ["-x", destination]);
  run("codesign", ["--force", "--sign", "-", destination]);
}

const metadataResult = run("cargo", ["metadata", "--no-deps", "--format-version", "1", "--manifest-path", manifest], { encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] });
const metadata = JSON.parse(metadataResult.stdout);
const members = new Set(metadata.workspace_members);
const binaries = metadata.packages.filter(pkg => members.has(pkg.id)).flatMap(pkg => pkg.targets.filter(target => target.kind.includes("bin")).map(target => target.name));
if (!binaries.includes("cito-native")) throw new Error("Cargo must declare the cito-native executable");
const destinations = new Map([["cito-native", [join(root, `bin/cito-native-${platform}`)]]]);
const pluginRoot = join(root, "plugins");
for (const entry of await readdir(pluginRoot, { withFileTypes: true })) {
  if (!entry.isDirectory()) continue;
  const path = join(pluginRoot, entry.name);
  let plugin;
  try { plugin = JSON.parse(await readFile(join(path, "plugin.json"), "utf8")); }
  catch (error) { if (error.code === "ENOENT") continue; throw error; }
  if (Array.isArray(plugin.command) && typeof plugin.command[0] === "string" && plugin.command[0].startsWith("./bin/")) {
    const name = plugin.command[0].slice("./bin/".length);
    if (name === "cito-ui") continue;
    if (!binaries.includes(name)) throw new Error(`No Cargo binary target for ${entry.name}: ${name}`);
    const output = resolve(path, plugin.command[0]);
    destinations.set(name, [...(destinations.get(name) ?? []), output]);
  }
  // Auxiliary executables belong to their plugin even when not its manifest entrypoint.
  for (const name of binaries.filter(name => name.startsWith(`cito-${entry.name}-`))) {
    destinations.set(name, [...(destinations.get(name) ?? []), join(path, "bin", name)]);
  }
}
for (const name of binaries) {
  if (!destinations.has(name)) throw new Error(`No package destination for Cargo binary ${name}`);
}

run("cargo", ["build", "--locked", "--release", "--bins", "--workspace", "--manifest-path", manifest, "--target-dir", targetDir]);
for (const [name, outputs] of destinations) {
  for (const output of new Set(outputs)) await install(join(targetDir, "release", name), output);
}
const ui = join(root, "bin/cito-ui-darwin-arm64");
run("swiftc", ["-O", "-target", "arm64-apple-macos13.0", join(root, "src/macos/cito-ui.swift"), "-o", ui]);
run("strip", ["-x", ui]);
run("codesign", ["--force", "--sign", "-", ui]);
await copyFile(ui, join(root, "plugins/human/bin/cito-ui"));
await chmod(join(root, "plugins/human/bin/cito-ui"), 0o755);
// Keep failed builds for diagnosis; remove only after all destinations exist and are signed.
if (process.env.CITO_KEEP_CARGO_TARGET !== "1") {
  await rm(targetDir, { recursive: true, force: true });
}
console.log(`Installed ${binaries.length} Rust targets and the macOS UI`);
