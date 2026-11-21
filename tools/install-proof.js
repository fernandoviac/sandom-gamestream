// The install proof of the GameStream server's npm packages (slice 5 of
// plans/PLAN-gamestream-runtimes.md). It packs the packages that
// tools/stage-packages.js staged for this machine's platform, installs them
// with npm into an empty directory from those archives alone (an empty
// cache, `--offline`), and then, under Deno, Bun and Node, imports the
// bindings from the install, finds the library of the installed platform
// package, starts the server on free local ports, reads /serverinfo and
// stops the server. Nothing is published and nothing is fetched.
//
//   deno run --allow-all tools/install-proof.js <platform> [runtimes]
//
// <platform> as for tools/stage-packages.js ("darwin-arm64", "linux-x64").
// [runtimes] is a comma-separated subset of deno,bun,node, all three by
// default; each must be on PATH.

import { gameStreamPlatforms } from "../bindings/library.js";

const repository = new URL("../", import.meta.url);
const staged = new URL("dist/npm/", repository);
const decoder = new TextDecoder();

const RUNTIMES = Object.freeze({
  deno: ["run", "--node-modules-dir=manual", "--allow-all", "probe.js"],
  bun: ["probe.js"],
  node: ["probe.js"],
});

const platformName = Deno.args[0]?.replace("-", " ");
const platform = gameStreamPlatforms[platformName];
const runtimes = (Deno.args[1] ?? "deno,bun,node").split(",");
if (platform === undefined || runtimes.some((runtime) => RUNTIMES[runtime] === undefined)) {
  throw new Error(`usage: install-proof.js <platform> [runtimes]; the platforms are ${Object.keys(gameStreamPlatforms).map((name) => name.replace(" ", "-")).join(", ")} and the runtimes ${Object.keys(RUNTIMES).join(",")}; observed ${JSON.stringify(Deno.args)}`);
}

async function run(command, args, cwd) {
  const result = await new Deno.Command(command, { args, cwd, stdout: "piped", stderr: "piped", stdin: "null" }).output();
  const stdout = decoder.decode(result.stdout);
  if (!result.success) throw new Error(`${command} ${args.join(" ")} in ${cwd} exited ${result.code}:\n${stdout}\n${decoder.decode(result.stderr)}`);
  return stdout;
}

// The probe that each runtime runs in the install. It prints one JSON line;
// the process then exits by itself, which shows that the stopped server
// keeps nothing alive.
const PROBE = `
import { mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { gameStreamLibrary, gameStreamRuntime, startGameStreamServer } from "@sandom/gamestream/bindings/mod.js";

const library = gameStreamLibrary();
if (library === null) throw new Error("no installed library of the GameStream server for this platform");
if (!library.pathname.includes("/node_modules/")) throw new Error("the library is not the installed package's: " + library.href);
const freePort = () => new Promise((resolve, reject) => {
  const listener = createServer();
  listener.on("error", reject);
  listener.listen(0, "127.0.0.1", () => {
    const { port } = listener.address();
    listener.close(() => resolve(port));
  });
});
const ports = {};
for (const name of ["http", "https", "rtsp", "video", "control", "audio"]) ports[name] = await freePort();
const certificateDirectory = mkdtempSync(join(tmpdir(), "gamestream-install-"));
try {
  const server = startGameStreamServer({
    hostName: "install-proof", certificateDirectory, applicationTitle: "Sandom", bindAddress: "127.0.0.1", ports,
  }, () => {});
  const serverInfo = await (await fetch("http://127.0.0.1:" + ports.http + "/serverinfo?uniqueid=0123456789ABCDEF")).text();
  await server.stop();
  for (const expected of ["<hostname>install-proof</hostname>", "<ServerCodecModeSupport>1</ServerCodecModeSupport>"]) {
    if (!serverInfo.includes(expected)) throw new Error("/serverinfo lacks " + expected + ": " + serverInfo);
  }
  console.log(JSON.stringify({ runtime: gameStreamRuntime, library: library.pathname }));
} finally {
  rmSync(certificateDirectory, { recursive: true, force: true });
}
`;

const work = await Deno.makeTempDir({ prefix: "gamestream-install" });
try {
  const archives = `${work}/archives`;
  const consumer = `${work}/consumer`;
  const cache = `${work}/cache`;
  await Deno.mkdir(archives);
  await Deno.mkdir(consumer);
  await Deno.mkdir(cache);
  const packed = [];
  for (const directory of ["gamestream/", `${platform.package.replace("@sandom/", "")}/`]) {
    const name = (await run("npm", ["pack", "--pack-destination", archives, new URL(directory, staged).pathname], work)).trim().split("\n").at(-1);
    packed.push(`${archives}/${name}`);
  }
  await Deno.writeTextFile(`${consumer}/package.json`, JSON.stringify({ name: "gamestream-install-proof", private: true, type: "module" }) + "\n");
  await run("npm", ["install", "--offline", "--cache", cache, "--no-audit", "--no-fund", ...packed], consumer);
  await Deno.writeTextFile(`${consumer}/probe.js`, PROBE);
  const results = [];
  for (const runtime of runtimes) {
    const result = JSON.parse((await run(runtime, RUNTIMES[runtime], consumer)).trim().split("\n").at(-1));
    if (result.runtime !== runtime) throw new Error(`${runtime} reported runtime ${result.runtime}`);
    results.push(result);
  }
  console.log(JSON.stringify({ platform: platformName, packages: packed.map((path) => path.slice(archives.length + 1)), results }));
} finally {
  await Deno.remove(work, { recursive: true });
}
