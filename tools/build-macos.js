// Builds the macOS libraries and Node addons of the GameStream server for
// both architectures, for the platform packages. Rust records source paths
// in panic locations; the build maps this repository to `sandom-gamestream` and
// Cargo's home to `cargo`, so the libraries name no directory of the build
// machine. The builds land in target/aarch64-apple-darwin/release/ and
// target/x86_64-apple-darwin/release/, which tools/stage-packages.js takes;
// tools/align-macho-string-table.js repairs each library the
// linker writes:
//
//   deno run --allow-read --allow-write --allow-run --allow-env tools/build-macos.js

import { alignStringTables } from "./align-macho-string-table.js";

const TARGETS = ["aarch64-apple-darwin", "x86_64-apple-darwin"];

if (Deno.build.os !== "darwin") throw new Error(`tools/build-macos.js builds on macOS; this machine runs ${Deno.build.os}`);
const repository = new URL("../", import.meta.url).pathname.replace(/\/$/, "");
const cargoHome = Deno.env.get("CARGO_HOME") ?? `${Deno.env.get("HOME")}/.cargo`;
const rustflags = [
  `--remap-path-prefix=${repository}=sandom-gamestream`,
  `--remap-path-prefix=${cargoHome}=cargo`,
].join(" ");

for (const target of TARGETS) {
  const status = await new Deno.Command("cargo", {
    args: ["build", "--locked", "--release", "--workspace", "--target", target],
    cwd: repository,
    env: { RUSTFLAGS: rustflags },
    stdout: "inherit", stderr: "inherit",
  }).spawn().status;
  if (!status.success) throw new Error(`cargo build for ${target} exited ${status.code}`);
  await alignStringTables([`${repository}/target/${target}/release`]);
}
console.log(JSON.stringify({ built: TARGETS.map((target) => `${repository}/target/${target}/release/`) }));
