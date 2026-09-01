// Native addons (Slint, sharp) and the packages that carry their own
// binaries (node-notifier) cannot be bundled into the executable: they
// are loaded from the node_modules folder shipped beside it. In
// development the same call resolves against the repository.
import { createRequire } from "node:module";
import { join } from "node:path";
import { runtimeRoot } from "./resources.ts";

// createRequire resolves relative to a file, so a manifest inside the
// unpacked runtime (or at the repository root) anchors the lookup.
const anchor = join(runtimeRoot(), "package.json");

export const nativeRequire = createRequire(anchor);
