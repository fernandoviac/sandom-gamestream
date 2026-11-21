// Stages the npm packages of the GameStream server (slice 5 of
// plans/PLAN-gamestream-runtimes.md) into dist/npm/:
//
// - `@sandom/gamestream`: the runtime bindings, which name each platform
//   package as an optional dependency at the same version, so that npm
//   installs the one of the machine;
// - one platform package, such as `@sandom/gamestream-darwin-arm64`: the
//   library, the Node addon beside it, and the third-party notices of the
//   crates that the library contains.
//
//   deno run --allow-read --allow-write --allow-run tools/stage-packages.js <platform> <built directory>
//
// <platform> is a key of bindings/library.js's platforms with the space as
// a hyphen ("darwin-arm64", "linux-x64"); <built directory> holds that
// platform's release build (target/release for the machine's own platform,
// target/<triple>/release for another). The tool stages the bindings
// package each time; it is the same for every platform. Nothing is packed
// or published.

import { gameStreamPlatforms } from "../bindings/library.js";
import { alignStringTables } from "./align-macho-string-table.js";

const repository = new URL("../", import.meta.url);
const output = new URL("dist/npm/", repository);

// Each platform's Rust target and npm platform fields.
const TARGETS = Object.freeze({
  "darwin arm64": { triple: "aarch64-apple-darwin", os: "darwin", cpu: "arm64" },
  "darwin x64": { triple: "x86_64-apple-darwin", os: "darwin", cpu: "x64" },
  "linux x64": { triple: "x86_64-unknown-linux-gnu", os: "linux", cpu: "x64", libc: "glibc" },
  "linux arm64": { triple: "aarch64-unknown-linux-gnu", os: "linux", cpu: "arm64", libc: "glibc" },
});
for (const name of Object.keys(gameStreamPlatforms)) {
  if (TARGETS[name] === undefined) throw new Error(`bindings/library.js names platform ${name}, which tools/stage-packages.js has no target for`);
}

const [platformArgument, builtArgument] = Deno.args;
const platformName = platformArgument?.replace("-", " ");
const platform = gameStreamPlatforms[platformName];
if (platform === undefined || builtArgument === undefined) {
  throw new Error(`usage: stage-packages.js <platform> <built directory>; the platforms are ${Object.keys(gameStreamPlatforms).map((name) => name.replace(" ", "-")).join(", ")}; observed ${JSON.stringify(Deno.args)}`);
}
const built = new URL(builtArgument.endsWith("/") ? builtArgument : `${builtArgument}/`, `file://${Deno.cwd()}/`);
const target = TARGETS[platformName];

// The version of the crate, which every package carries; package.json, which
// names the release for the publication tools, must carry the same.
const manifest = await Deno.readTextFile(new URL("Cargo.toml", repository));
const version = manifest.match(/^version = "([^"]+)"$/m)?.[1];
if (version === undefined) throw new Error("Cargo.toml names no package version");
const { version: packageVersion, license, repository: sourceRepository } = JSON.parse(
  await Deno.readTextFile(new URL("package.json", repository)),
);
if (packageVersion !== version) {
  throw new Error(`Cargo.toml has version ${version} and package.json ${packageVersion}; give both the release's version`);
}

const decoder = new TextDecoder();
async function run(command, args) {
  const result = await new Deno.Command(command, { args, cwd: repository, stdout: "piped", stderr: "piped" }).output();
  if (!result.success) throw new Error(`${command} ${args.join(" ")} exited ${result.code}:\n${decoder.decode(result.stderr)}`);
  return result.stdout;
}

async function writePackage(directory, packageJson, files) {
  await Deno.remove(directory, { recursive: true }).catch((error) => {
    if (!(error instanceof Deno.errors.NotFound)) throw error;
  });
  await Deno.mkdir(directory, { recursive: true });
  for (const [name, source] of files) {
    const destination = new URL(name, directory);
    await Deno.mkdir(new URL("./", destination), { recursive: true });
    await Deno.copyFile(source, destination);
  }
  await Deno.writeTextFile(new URL("package.json", directory), JSON.stringify(packageJson, null, 2) + "\n");
}

const common = {
  version,
  license,
  homepage: "https://cdn.urania-libs.com",
  ...(sourceRepository === undefined ? {} : { repository: sourceRepository }),
  author: "DevBlanket AB (https://x.com/fernandoviac)",
};
// The Apache-2.0 code written for Sandom, the BSD 2-Clause code from
// Moonshine, and the notice that names both.
const legal = ["LICENSE", "LICENSE-MOONSHINE", "NOTICE"].map((name) => [name, new URL(name, repository)]);

// The bindings package.
const bindings = [];
for await (const entry of Deno.readDir(new URL("bindings/", repository))) {
  if (entry.isFile && entry.name.endsWith(".js")) bindings.push([`bindings/${entry.name}`, new URL(`bindings/${entry.name}`, repository)]);
}
bindings.sort(([left], [right]) => left.localeCompare(right));
await writePackage(new URL("gamestream/", output), {
  name: "@sandom/gamestream",
  ...common,
  description: "A GameStream server for Moonlight clients, for Deno, Bun and Node.",
  type: "module",
  exports: { "./bindings/*.js": "./bindings/*.js" },
  optionalDependencies: Object.fromEntries(Object.values(gameStreamPlatforms).map(({ package: name }) => [name, version])),
}, [...bindings, ...legal]);

// The platform package.
const addon = platform.file.replace("libsandom_gamestream.", "libsandom_gamestream_node.");
for (const file of [platform.file, addon]) {
  try {
    await Deno.stat(new URL(file, built));
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) {
      throw new Error(`${new URL(file, built).pathname} is not built; run \`cargo build --release --workspace${builtArgument.includes(target.triple) ? ` --target ${target.triple}` : ""}\``);
    }
    throw error;
  }
}
const notices = new URL(`notices-${target.triple}.txt`, output);
await Deno.mkdir(output, { recursive: true });
await run("cargo", ["about", "generate", "--locked", "--target", target.triple, "about.hbs", "-o", notices.pathname]);
const platformDirectory = new URL(`${platform.package.replace("@sandom/", "")}/`, output);
await writePackage(platformDirectory, {
  name: platform.package,
  ...common,
  description: `The native library of Sandom's GameStream server for ${platformName}.`,
  os: [target.os],
  cpu: [target.cpu],
  ...(target.libc === undefined ? {} : { libc: [target.libc] }),
}, [
  [platform.file, new URL(platform.file, built)],
  [addon, new URL(addon, built)],
  ["THIRD-PARTY-NOTICES.txt", notices],
  ...legal,
]);
await Deno.remove(notices);
// Each macOS library names itself by `@rpath` instead of the absolute path
// of the build directory, which the linker records. It then carries an
// ad-hoc signature: Apple Silicon loads no unsigned code, and the linker
// signs arm64 output only (and the new name voids its signature). An
// ad-hoc signature names no developer and costs nothing; npm marks no
// installed file as downloaded, so macOS asks for no Developer ID
// signature.
if (target.os === "darwin") {
  for (const file of [platform.file, addon]) {
    const path = new URL(file, platformDirectory).pathname;
    await run("install_name_tool", ["-id", `@rpath/${file}`, path]);
    await alignStringTables([path]);
    await run("codesign", ["--force", "--sign", "-", path]);
  }
}

console.log(JSON.stringify({
  version,
  bindings: new URL("gamestream/", output).pathname,
  platform: platformDirectory.pathname,
}));
