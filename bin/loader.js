// Resolve prebuilt dist/ first, falling back to Node's native TypeScript runtime relative to this installation.
import { existsSync } from "node:fs";
import { register } from "node:module";
import { fileURLToPath } from "node:url";

function makeLoaderHookSource(sdkUrl) {
  return `import { readFile } from "node:fs/promises";
import { stripTypeScriptTypes } from "node:module";
import { fileURLToPath } from "node:url";
process.emitWarning = () => {};
const SDK_URL = ${JSON.stringify(sdkUrl)};
export async function resolve(specifier, context, nextResolve) {
  if (specifier === "@tefx/cito/plugin-sdk") {
    return { url: SDK_URL, shortCircuit: true };
  }
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
  const cleanUrl = url.split("?")[0];
  if (cleanUrl.endsWith(".ts")) {
    const source = await readFile(fileURLToPath(cleanUrl), "utf8");
    return {
      format: "module",
      shortCircuit: true,
      source: stripTypeScriptTypes(source, { mode: "strip", sourceUrl: url }),
    };
  }
  return nextLoad(url, context);
}`;
}

let hookRegistered = false;

export async function runEntrypoint(distRelPath, srcRelPath) {
  const distEntry = new URL(distRelPath, import.meta.url);
  const srcEntry = new URL(srcRelPath, import.meta.url);
  const useDist = existsSync(fileURLToPath(distEntry));
  if (!hookRegistered && !globalThis.__citoPluginHookRegistered) {
    hookRegistered = true;
    globalThis.__citoPluginHookRegistered = true;
    const origEmitWarning = process.emitWarning;
    process.emitWarning = (warning, ...args) => {
      const msg = typeof warning === "string" ? warning : warning?.message ?? "";
      if (msg.includes("stripTypeScriptTypes") || msg.includes("module.register()")) return;
      return origEmitWarning.call(process, warning, ...args);
    };
    const sdkUrl = new URL(
      useDist ? "../dist/src/adapters/index.js" : "../src/adapters/index.ts",
      import.meta.url,
    ).href;
    register(`data:text/javascript,${encodeURIComponent(makeLoaderHookSource(sdkUrl))}`, import.meta.url);
  }
  const { main } = await import(useDist ? distEntry.href : srcEntry.href);
  await main();
}
