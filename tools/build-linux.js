// Builds the Linux library and Node addon of the GameStream server for the
// machine's own architecture, inside a Debian bookworm container, so that
// they need glibc 2.36 rather than the build machine's. Run it on an arm64
// machine for linux-arm64 (Docker on Apple Silicon runs arm64 Linux
// natively) and on an x64 machine for linux-x64. The build lands in
// target/linux-<architecture>/release/, which tools/stage-packages.js takes:
//
//   deno run --allow-read --allow-run tools/build-linux.js
//   deno run --allow-all tools/stage-packages.js linux-<architecture> target/linux-<architecture>/release
//
// The Rust version is the one of IMAGE, the same as the macOS builds'.

const IMAGE = "rust:1.92-bookworm";
const ARCHITECTURES = Object.freeze({ aarch64: "arm64", x86_64: "x64" });

const architecture = ARCHITECTURES[Deno.build.arch];
if (architecture === undefined) throw new Error(`tools/build-linux.js builds for arm64 and x64; this machine is ${Deno.build.arch}`);
const repository = new URL("../", import.meta.url).pathname;
const targetDirectory = `target/linux-${architecture}`;

const script = [
  "set -eu",
  `cargo build --locked --release --workspace --target-dir ${targetDirectory}`,
  // The files belong to the repository's owner as the container sees it:
  // the host user under rootful Docker, and the container's root under
  // rootless Docker, which is the host user outside.
  `chown -R "$(stat -c %u:%g /repository)" ${targetDirectory}`,
  `ldd --version | head -1`,
].join("\n");

const status = await new Deno.Command("docker", {
  args: [
    "run", "--rm", "--platform", `linux/${architecture === "x64" ? "amd64" : "arm64"}`,
    "--volume", `${repository}:/repository`,
    "--volume", "sandom-gamestream-cargo-registry:/usr/local/cargo/registry",
    "--workdir", "/repository",
    IMAGE, "bash", "-c", script,
  ],
  stdout: "inherit", stderr: "inherit",
}).spawn().status;
if (!status.success) throw new Error(`the Linux build in ${IMAGE} exited ${status.code}`);
console.log(JSON.stringify({ architecture, built: `${repository}${targetDirectory}/release/` }));
