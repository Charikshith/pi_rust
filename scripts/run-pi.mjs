#!/usr/bin/env node
// Run REAL pi (TypeScript source, no dist build) as a subprocess entry point.
//
// Registers the same bare-specifier alias hooks the other gen-*-oracle scripts
// use, then hands control to packages/coding-agent/src/cli.ts. Everything after
// "--" on our argv is forwarded to pi verbatim:
//
//   node scripts/run-pi.mjs -- --mode rpc --provider anthropic --model claude-opus-4-8
//
// The ../pi checkout is never modified.

import { existsSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL, fileURLToPath, URL } from "node:url";
import { sep } from "node:path";
import { register } from "node:module";

const here = fileURLToPath(import.meta.url);
const pirustRoot = join(here, "..", "..");
const PKGS = join(pirustRoot, "..", "pi", "packages");
const CA = join(PKGS, "coding-agent", "src");

if (!existsSync(join(CA, "cli.ts"))) {
	console.error(`pi sources not found at ${CA}`);
	process.exit(1);
}

const PKG_ROOTS = {
	"@earendil-works/pi-ai": join(PKGS, "ai", "src"),
	"@earendil-works/pi-agent-core": join(PKGS, "agent", "src"),
	"@earendil-works/pi-tui": join(PKGS, "tui", "src"),
	"@earendil-works/pi-telemetry": join(PKGS, "telemetry", "src"),
};

const roots = Object.fromEntries(
	Object.entries(PKG_ROOTS)
		.filter(([, dir]) => existsSync(dir))
		.map(([spec, dir]) => [spec, pathToFileURL(dir + sep).href]),
);

register(
	"data:text/javascript," +
		encodeURIComponent(`
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
const ROOTS = ${JSON.stringify(roots)};
export async function resolve(specifier, context, nextResolve) {
  for (const [pkg, rootUrl] of Object.entries(ROOTS)) {
    if (specifier === pkg) return { url: new URL("index.ts", rootUrl).href, shortCircuit: true };
    if (specifier.startsWith(pkg + "/")) {
      const rest = specifier.slice(pkg.length + 1);
      for (const cand of [rest + ".ts", rest + "/index.ts"]) {
        const u = new URL(cand, rootUrl);
        if (existsSync(fileURLToPath(u))) return { url: u.href, shortCircuit: true };
      }
      throw new Error("alias hook: no source file for " + specifier);
    }
  }
  return nextResolve(specifier, context);
}
`),
	import.meta.url,
);

const dd = process.argv.indexOf("--");
process.argv = [process.argv[0], "pi", ...(dd === -1 ? [] : process.argv.slice(dd + 1))];

await import(pathToFileURL(join(CA, "cli.ts")).href);
