// The native library of this platform: the one of its installed platform
// package, or the one built in this repository's release target. The Node
// addon lies beside the library under the name that `nodeAddon` gives.

import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";

// Each platform: its npm package and the library's file name.
const PLATFORMS = Object.freeze({
  "darwin arm64": { package: "@sandom/gamestream-darwin-arm64", file: "libsandom_gamestream.dylib" },
  "darwin x64": { package: "@sandom/gamestream-darwin-x64", file: "libsandom_gamestream.dylib" },
  "linux x64": { package: "@sandom/gamestream-linux-x64-gnu", file: "libsandom_gamestream.so" },
  "linux arm64": { package: "@sandom/gamestream-linux-arm64-gnu", file: "libsandom_gamestream.so" },
});

// Deno names the architectures as Rust does; Node as PLATFORMS does.
const ARCHITECTURES = Object.freeze({ aarch64: "arm64", x86_64: "x64" });

function platformName() {
  if (globalThis.Deno !== undefined) return `${Deno.build.os} ${ARCHITECTURES[Deno.build.arch] ?? Deno.build.arch}`;
  return `${globalThis.process.platform} ${globalThis.process.arch}`;
}

const platform = PLATFORMS[platformName()] ?? null;

export const gameStreamPlatforms = PLATFORMS;

// The file URL of the library in this platform's installed package, or
// null when the package is not installed or the platform has none. An
// installed package lies in a node_modules directory: Bun's auto-install
// resolves a package that no project installs to its global cache, whose
// version need not match these bindings.
export function installedLibrary() {
  if (platform === null || new URL(import.meta.url).protocol !== "file:") return null;
  let location;
  try {
    location = createRequire(import.meta.url).resolve(`${platform.package}/${platform.file}`);
  } catch (error) {
    if (error?.code === "MODULE_NOT_FOUND") return null;
    throw error;
  }
  if (!/[\\/]node_modules[\\/]/.test(location)) return null;
  return pathToFileURL(location);
}

// The file URL of the library in this repository's release target, whether
// or not it is built, or null for a platform without a package.
export function checkoutLibrary() {
  return platform === null ? null : new URL(`../target/release/${platform.file}`, import.meta.url);
}

// The Node addon beside the library at `library` (a file URL).
export function nodeAddon(library) {
  return new URL(library.href.replace(/libsandom_gamestream\.(dylib|so)$/, "libsandom_gamestream_node.$1"));
}
