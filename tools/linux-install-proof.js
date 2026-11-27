// Runs tools/install-proof.js for the machine's own Linux architecture inside
// a Debian bookworm container, such as Docker on Apple Silicon for
// linux-arm64. The server needs no GPU.
// The container installs Deno and Bun beside the image's Node;
// tools/stage-packages.js must have staged the platform's package first:
//
//   deno run --allow-read --allow-run tools/linux-install-proof.js
//
// On a Linux machine with Deno, Bun and Node, run tools/install-proof.js
// there instead.

const IMAGE = "node:22-bookworm";
const ARCHITECTURES = Object.freeze({ aarch64: "arm64", x86_64: "x64" });

const architecture = ARCHITECTURES[Deno.build.arch];
if (architecture === undefined) throw new Error(`tools/linux-install-proof.js runs for arm64 and x64; this machine is ${Deno.build.arch}`);
const repository = new URL("../", import.meta.url).pathname;

const script = [
  "set -eu",
  "apt-get update -qq",
  "apt-get install -qq -y --no-install-recommends unzip curl ca-certificates >/dev/null",
  "(cd /tmp && curl -fsSL https://deno.land/install.sh | sh -s -- --yes >/dev/null)",
  "(cd /tmp && curl -fsSL https://bun.sh/install | bash >/dev/null 2>&1)",
  "export PATH=\"$HOME/.deno/bin:$HOME/.bun/bin:$PATH\"",
  "echo \"deno $(deno --version | head -1 | cut -d' ' -f2) bun $(bun --version) node $(node --version)\"",
  // The repository is mounted read-only: Deno writes no lockfile there.
  `deno run --no-lock --allow-all tools/install-proof.js linux-${architecture}`,
].join("\n");

const status = await new Deno.Command("docker", {
  args: [
    "run", "--rm", "--platform", `linux/${architecture === "x64" ? "amd64" : "arm64"}`,
    "--volume", `${repository}:/repository:ro`,
    "--workdir", "/repository",
    IMAGE, "bash", "-c", script,
  ],
  stdout: "inherit", stderr: "inherit",
}).spawn().status;
if (!status.success) throw new Error(`the install proof in ${IMAGE} exited ${status.code}`);
