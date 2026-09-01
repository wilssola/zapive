// Packages Zapive into a native executable for the host platform.
//
//   bun run build            -> dist/Zapive/
//
// The app's own code is bundled with esbuild and injected into a copy of
// the Node runtime (Node's Single Executable Application format), so the
// result runs without Node installed. What cannot be bundled — the native
// addons, the Slint markup and the icons it reads from disk — is packed
// into one compressed archive that rides inside the executable as an SEA
// asset and is unpacked into the user's cache on first run.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";

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
// The contents go, not the directory itself: a file manager or an editor
// holding dist/ open would otherwise block the whole build.
function clearDir(dir) {
  if (!existsSync(dir)) return;
  for (const entry of readdirSync(dir)) {
    rmSync(join(dir, entry), {
      recursive: true,
      force: true,
      maxRetries: 5,
      retryDelay: 200,
    });
  }
}
try {
  clearDir(OUT);
  clearDir(WORK);
} catch (err) {
  if (err.code === "EPERM" || err.code === "EBUSY" || err.code === "ENOTEMPTY") {
    console.error(
      "\n   A packaged Zapive is still running and holds its own file open." +
        "\n   Close it (window and tray icon) and run the build again.",
    );
    process.exit(1);
  }
  throw err;
}
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

// ---- 4. Collect what has to exist as real files at runtime ----
//
// Native addons cannot be loaded from memory (dlopen needs a path), and
// Slint reads the markup and the icons from disk, so these are gathered
// here and packed in the next step.
step("collecting runtime files");
const NM = join(ROOT, "node_modules");

// Prebuilt binaries for the other operating systems are dead weight.
const FOREIGN = ["win32", "darwin", "linux"].filter((p) => p !== process.platform);

/** @type {{ path: string, data: Buffer, mode: number }[]} */
const files = [];

function addFile(abs, rel) {
  files.push({
    path: rel.split(sep).join("/"),
    data: readFileSync(abs),
    mode: statSync(abs).mode,
  });
}

function addTree(absDir, relDir, skip = () => false) {
  for (const entry of readdirSync(absDir, { withFileTypes: true })) {
    const abs = join(absDir, entry.name);
    const rel = join(relDir, entry.name);
    if (skip(rel)) continue;
    if (entry.isDirectory()) addTree(abs, rel, skip);
    else if (entry.isFile()) addFile(abs, rel);
  }
}

const seen = new Set();
function addPackage(name) {
  if (seen.has(name)) return;
  seen.add(name);
  const from = join(NM, ...name.split("/"));
  if (!existsSync(from)) return;
  addTree(from, join("node_modules", ...name.split("/")), (rel) => {
    const posix = rel.split(sep).join("/");
    return FOREIGN.some((p) => posix.includes("-" + p + "-") || posix.includes("/" + p + "-"));
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
    addPackage(dep);
  }
}

for (const name of EXTERNAL) addPackage(name);
addTree(join(ROOT, "ui"), "ui");
addTree(join(ROOT, "i18n"), "i18n");
files.push({
  path: "package.json",
  data: Buffer.from(JSON.stringify({ name: "zapive", private: true }, null, 2)),
  mode: 0o644,
});
console.log(`   ${seen.size} packages, ${files.length} files`);

// ---- 5. Pack them: magic, manifest, gzipped payload (see resources.ts) ----
step("packing resources");
let offset = 0;
const manifest = { id: "", files: [] };
for (const f of files) {
  manifest.files.push({ p: f.path, o: offset, n: f.data.length, m: f.mode });
  offset += f.data.length;
}
const payload = Buffer.concat(files.map((f) => f.data));
// The id names the unpack directory, so a new build never reuses an old one.
manifest.id = createHash("sha256")
  .update(JSON.stringify(manifest.files))
  .update(payload)
  .digest("hex")
  .slice(0, 16);
const header = Buffer.from(JSON.stringify(manifest), "utf8");
const headerLen = Buffer.alloc(4);
headerLen.writeUInt32LE(header.length);
const compressed = gzipSync(payload, { level: 9 });
const packPath = join(WORK, "resources.pack");
writeFileSync(
  packPath,
  Buffer.concat([Buffer.from("ZPAK1", "utf8"), headerLen, header, compressed]),
);
console.log(
  "   " +
    (payload.length / 1e6).toFixed(1) +
    " MB -> " +
    (compressed.length / 1e6).toFixed(1) +
    " MB compressed",
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
    assets: { "resources.pack": packPath },
  }),
);
sh(process.execPath, ["--experimental-sea-config", seaConfig]);
copyFileSync(process.execPath, BIN);

// The application icon must land before the blob: rcedit rewrites the
// PE resource table, which would displace an already-injected asset.
if (process.platform === "win32") {
  const { rcedit } = await import("rcedit");
  await rcedit(BIN, {
    icon: join(ROOT, "ui", "zapive.ico"),
    "version-string": { ProductName: "Zapive", FileDescription: "Zapive" },
  });
  console.log("   icon applied");
}

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

const mb = (statSync(BIN).size / 1e6).toFixed(0);
step(`done: ${BIN} (${mb} MB)`);
console.log(
  "   One file: the UI, the catalogs and the native packages ride" +
    " inside it and are unpacked into the user's cache on first run." +
    " Data goes to the OS profile directory, never beside the binary.",
);
