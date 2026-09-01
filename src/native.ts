// Native addons (Slint, sharp) and the packages that carry their own
// binaries (node-notifier) cannot be bundled into the executable: they
// are loaded from the node_modules folder shipped beside it. In
// development the same call resolves against the repository.
import { createRequire } from "node:module";
import { join } from "node:path";
import { isPackaged, resourceRoot } from "./paths.ts";

// createRequire resolves relative to a file, so the manifest beside the
// executable (or at the repository root) anchors the lookup.
const anchor = join(resourceRoot(), "package.json");

export const nativeRequire = createRequire(anchor);
