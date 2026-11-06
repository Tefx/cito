// Resolve prebuilt dist/ first, falling back to Node's native TypeScript runtime relative to this installation.
import { existsSync } from "node:fs";
import { register } from "node:module";
import { fileURLToPath } from "node:url";

const tsExtensionHook = `import { readFile } from "node:fs/promises";
import { stripTypeScriptTypes } from "node:module";
import { fileURLToPath } from "node:url";
process.emitWarning = () => {};
export async function resolve(specifier, context, nextResolve) {
  try {
    return await nextResolve(specifier, context);
  } catch (error) {
    if (error?.code === "ERR_MODULE_NOT_FOUND" && /^\\.\\.?\\/.*\\.js$/.test(specifier)) {
      return nextResolve(specifier.slice(0, -3) + ".ts", context);
    }
    throw error;
  }
}
export async function load(url, context, nextLoad) {
  if (url.endsWith(".ts")) {
    const source = await readFile(fileURLToPath(url), "utf8");
    return {
      format: "module",
      shortCircuit: true,
      source: stripTypeScriptTypes(source, { mode: "strip", sourceUrl: url }),
    };
  }
  return nextLoad(url, context);
}`;

let hookRegistered = false;

export async function runEntrypoint(distRelPath, srcRelPath) {
  const distEntry = new URL(distRelPath, import.meta.url);
  const srcEntry = new URL(srcRelPath, import.meta.url);
  if (existsSync(fileURLToPath(distEntry))) {
    const { main } = await import(distEntry.href);
    await main();
    return;
  }
  if (!hookRegistered) {
    hookRegistered = true;
    const origEmitWarning = process.emitWarning;
    process.emitWarning = (warning, ...args) => {
      const msg = typeof warning === "string" ? warning : warning?.message ?? "";
      if (msg.includes("stripTypeScriptTypes") || msg.includes("module.register()")) return;
      return origEmitWarning.call(process, warning, ...args);
    };
    register(`data:text/javascript,${encodeURIComponent(tsExtensionHook)}`, import.meta.url);
  }
  const { main } = await import(srcEntry.href);
  await main();
}
