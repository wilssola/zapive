// Packages Zapive into a native executable for the host platform.
//
//   bun run build            -> dist/Zapive/
//
// The app's own code is bundled with esbuild and injected into a copy of
// the Node runtime (Node's Single Executable Application format), so the
// result runs without Node installed. Three packages cannot be bundled
// because they carry native binaries — slint-ui, sharp and node-notifier
// — so they are copied next to the executable together with the UI and
// the translation catalogs.
import { execFileSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const OUT = join(ROOT, "dist", "Zapive");
const WORK = join(ROOT, "dist", ".build");
const EXE = process.platform === "win32" ? ".exe" : "";
const BIN = join(OUT, `Zapive${EXE}`);

// Packages that must stay on disk: native addons and vendored binaries.
const EXTERNAL = ["slint-ui", "sharp", "node-notifier"];

function step(msg) {
  console.log(`\n[36m==[0m ${msg}`);
}

function sh(cmd, args, opts = {}) {
  execFileSync(cmd, args, { stdio: "inherit", cwd: ROOT, ...opts });
}

// ---- 1. Clean ----
step("cleaning dist/");
rmSync(join(ROOT, "dist"), { recursive: true, force: true });
mkdirSync(OUT, { recursive: true });
mkdirSync(WORK, { recursive: true });

// ---- 2. Translations ----
step("compiling translation catalogs");
sh(process.execPath, [join(ROOT, "scripts", "build-mo.mjs")]);

// ---- 3. Bundle the app ----
step("bundling the application");
const esbuild = join(
  ROOT,
  "node_modules",
  ".bin",
  process.platform === "win32" ? "esbuild.exe" : "esbuild",
);
const bundle = join(OUT, "zapive.cjs");
sh(esbuild, [
  join(ROOT, "src", "main.ts"),
  "--bundle",
  "--platform=node",
  "--format=cjs",
  "--target=node24",
  "--loader:.ts=ts",
  ...EXTERNAL.map((p) => `--external:${p}`),
  `--outfile=${bundle}`,
]);

// ---- 4. Copy the packages that stay external, with their dependencies ----
step("copying native packages");
const NM = join(ROOT, "node_modules");
const destNM = join(OUT, "node_modules");

// Binaries for other platforms are dead weight in a per-platform build.
const platformTag = {
  win32: "win32",
  darwin: "darwin",
  linux: "linux",
}[process.platform];

function copyPackage(name, seen) {
  if (seen.has(name)) return;
  seen.add(name);
  const from = join(NM, ...name.split("/"));
  if (!existsSync(from)) return;
  const to = join(destNM, ...name.split("/"));
  mkdirSync(dirname(to), { recursive: true });
  cpSync(from, to, {
    recursive: true,
    dereference: true,
    filter: (src) => {
      // Skip prebuilt binaries for the other operating systems.
      const rel = src.slice(from.length).replace(/\\/g, "/");
      const foreign = ["win32", "darwin", "linux"].filter((p) => p !== platformTag);
      return !foreign.some((p) => rel.includes(`-${p}-`) || rel.includes(`/${p}-`));
    },
  });
  let pkg;
  try {
    pkg = JSON.parse(readFileSync(join(from, "package.json"), "utf8"));
  } catch {
    return;
  }
  for (const dep of Object.keys({
    ...(pkg.dependencies ?? {}),
    ...(pkg.optionalDependencies ?? {}),
  })) {
    copyPackage(dep, seen);
  }
}

const seen = new Set();
for (const name of EXTERNAL) copyPackage(name, seen);
console.log(`   ${seen.size} packages`);

// ---- 5. Resources ----
step("copying resources");
cpSync(join(ROOT, "ui"), join(OUT, "ui"), { recursive: true });
cpSync(join(ROOT, "i18n"), join(OUT, "i18n"), {
  recursive: true,
  filter: (src) => !src.endsWith(".po") || true,
});
writeFileSync(
  join(OUT, "package.json"),
  JSON.stringify({ name: "zapive", private: true, main: "zapive.cjs" }, null, 2),
);

// ---- 6. Single executable ----
step("building the executable");
const seaConfig = join(WORK, "sea-config.json");
const blob = join(WORK, "zapive.blob");
writeFileSync(
  seaConfig,
  JSON.stringify({
    main: bundle,
    output: blob,
    disableExperimentalSEAWarning: true,
    useSnapshot: false,
    useCodeCache: false,
  }),
);
sh(process.execPath, ["--experimental-sea-config", seaConfig]);
cpSync(process.execPath, BIN);

// Windows and macOS refuse to run a binary whose signature no longer
// matches once the blob is injected, so the signature is dropped first.
if (process.platform === "win32") {
  try {
    sh("signtool", ["remove", "/s", BIN], { stdio: "ignore" });
  } catch {
    console.log("   (signtool not available — continuing unsigned)");
  }
} else if (process.platform === "darwin") {
  try {
    sh("codesign", ["--remove-signature", BIN]);
  } catch {
    console.log("   (codesign unavailable — continuing)");
  }
}

const postject = join(
  ROOT,
  "node_modules",
  ".bin",
  process.platform === "win32" ? "postject.exe" : "postject",
);
const injectArgs = [
  BIN,
  "NODE_SEA_BLOB",
  blob,
  "--sentinel-fuse",
  "NODE_SEA_FUSE_fce680ab2cc467b6e072b8b5df1996b2",
];
if (process.platform === "darwin") injectArgs.push("--macho-segment-name", "NODE_SEA");
sh(postject, injectArgs);

if (process.platform === "darwin") {
  try {
    sh("codesign", ["--sign", "-", BIN]);
  } catch {
    console.log("   (ad-hoc signing failed — the app may be blocked by Gatekeeper)");
  }
}

// The bundle now lives inside the executable.
rmSync(bundle, { force: true });
rmSync(WORK, { recursive: true, force: true });

step(`done: ${BIN}`);
console.log(
  "   The folder is self-contained: copy dist/Zapive anywhere.\n" +
    "   User data goes to the OS profile directory, never next to the binary.",
);
