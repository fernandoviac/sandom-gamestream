// Aligns the string table of each 64-bit Mach-O library to 8 bytes and signs
// it ad hoc. ld 27037 (Xcode 27) writes the string table right after the
// indirect symbol table without padding: with an odd number of indirect
// symbols the table starts at an offset of 4 modulo 8, and dyld refuses the
// library ("mis-aligned LINKEDIT string pool"). Whether a build loads then
// depends on how many symbols it imports. The repair inserts 4 zero bytes
// before the string table and moves every offset at or past it.
//
//   deno run --allow-read --allow-write --allow-run=codesign tools/align-macho-string-table.js <directory or library>...
//
// A directory names the *.dylib files directly inside it; a file that is not
// a 64-bit little-endian Mach-O (a Linux build) is left as it is.
//
// The same tool as sandom-gpu-host's tools/align-macho-string-table.js: the
// packaging stages from a copy of HEAD beside no other checkout, so each
// repository carries it. A change to one belongs in both.

const MH_MAGIC_64 = 0xfeedfacf;
const LC_SEGMENT_64 = 0x19;
const LC_SYMTAB = 0x2;
const LC_DYSYMTAB = 0xb;
const LC_CODE_SIGNATURE = 0x1d;
const LC_DYLD_INFO = 0x22;
const LC_DYLD_INFO_ONLY = 0x80000022;
// linkedit_data_command: cmd, cmdsize, dataoff, datasize.
const LINKEDIT_DATA_COMMANDS = new Set([
  0x1e, // LC_SEGMENT_SPLIT_INFO
  0x26, // LC_FUNCTION_STARTS
  0x29, // LC_DATA_IN_CODE
  0x2b, // LC_DYLIB_CODE_SIGN_DRS
  0x2e, // LC_LINKER_OPTIMIZATION_HINT
  0x80000033, // LC_DYLD_EXPORTS_TRIE
  0x80000034, // LC_DYLD_CHAINED_FIXUPS
]);
// The byte offsets of the file offsets in dysymtab_command and
// dyld_info_command; each offset field is followed by its count or size.
const DYSYMTAB_OFFSETS = [32, 40, 48, 56, 64, 72];
const DYLD_INFO_OFFSETS = [8, 16, 24, 32, 40];
const PAGE = 0x4000;

async function run(command, args) {
  const { success, code, stderr } = await new Deno.Command(command, { args, stderr: "piped" }).output();
  if (!success) throw new Error(`${command} ${args.join(" ")} exited ${code}: ${new TextDecoder().decode(stderr)}`);
}

/** The load commands of `bytes`, or undefined for a file that is no 64-bit Mach-O. */
function loadCommands(bytes) {
  if (bytes.length < 32) return undefined;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (view.getUint32(0, true) !== MH_MAGIC_64) return undefined;
  const commands = [];
  let at = 32;
  for (let index = 0, count = view.getUint32(16, true); index < count; index++) {
    commands.push({ at, cmd: view.getUint32(at, true), size: view.getUint32(at + 4, true) });
    at += view.getUint32(at + 4, true);
  }
  return { view, commands };
}

function stringTableOffset(path, parsed) {
  const symtab = parsed.commands.find(({ cmd }) => cmd === LC_SYMTAB);
  if (symtab === undefined) throw new Error(`${path} has no LC_SYMTAB load command`);
  return { symtab, offset: parsed.view.getUint32(symtab.at + 16, true) };
}

/** Aligns the string table of the library at `path`; true if it moved. */
async function align(path) {
  let bytes = await Deno.readFile(path);
  let parsed = loadCommands(bytes);
  if (parsed === undefined) return false;
  if (stringTableOffset(path, parsed).offset % 8 === 0) return false;
  if (parsed.commands.some(({ cmd }) => cmd === LC_CODE_SIGNATURE)) {
    await run("codesign", ["--remove-signature", path]);
    bytes = await Deno.readFile(path);
    parsed = loadCommands(bytes);
  }
  const { view, commands } = parsed;
  const { symtab, offset } = stringTableOffset(path, parsed);
  const size = view.getUint32(symtab.at + 20, true);
  if (offset % 8 !== 4) throw new Error(`${path} starts its string table at ${offset}, ${offset % 8} modulo 8; the repair inserts 4 bytes and expects 4`);
  if (offset + size !== bytes.length) {
    throw new Error(`${path} ends its string table at ${offset + size} of ${bytes.length} bytes; the repair expects the string table last`);
  }
  const moveField = (fieldAt) => {
    const value = view.getUint32(fieldAt, true);
    if (value >= offset) view.setUint32(fieldAt, value + 4, true);
  };
  let linkedit;
  for (const command of commands) {
    if (command.cmd === LC_SYMTAB) view.setUint32(command.at + 16, offset + 4, true);
    else if (command.cmd === LC_DYSYMTAB) DYSYMTAB_OFFSETS.forEach((field) => moveField(command.at + field));
    else if (command.cmd === LC_DYLD_INFO || command.cmd === LC_DYLD_INFO_ONLY) DYLD_INFO_OFFSETS.forEach((field) => moveField(command.at + field));
    else if (LINKEDIT_DATA_COMMANDS.has(command.cmd)) moveField(command.at + 8);
    else if (command.cmd === LC_SEGMENT_64) {
      const name = new TextDecoder().decode(bytes.subarray(command.at + 8, command.at + 24)).replace(/\0+$/, "");
      if (name === "__LINKEDIT") linkedit = command;
    }
  }
  if (linkedit === undefined) throw new Error(`${path} has no __LINKEDIT segment`);
  const fileSize = view.getBigUint64(linkedit.at + 48, true) + 4n;
  view.setBigUint64(linkedit.at + 48, fileSize, true);
  const vmSize = view.getBigUint64(linkedit.at + 32, true);
  if (fileSize > vmSize) view.setBigUint64(linkedit.at + 32, (fileSize + BigInt(PAGE - 1)) / BigInt(PAGE) * BigInt(PAGE), true);
  const repaired = new Uint8Array(bytes.length + 4);
  repaired.set(bytes.subarray(0, offset), 0);
  repaired.set(bytes.subarray(offset), offset + 4);
  await Deno.writeFile(path, repaired);
  await run("codesign", ["--force", "--sign", "-", path]);
  const after = stringTableOffset(path, loadCommands(await Deno.readFile(path))).offset;
  if (after % 8 !== 0) throw new Error(`${path} starts its string table at ${after} after the repair, ${after % 8} modulo 8`);
  return true;
}

/** Aligns each library named by `paths`, directories by their *.dylib files. */
export async function alignStringTables(paths) {
  const aligned = [];
  for (const path of paths) {
    const files = (await Deno.stat(path)).isDirectory
      ? [...Deno.readDirSync(path)].filter(({ isFile, name }) => isFile && name.endsWith(".dylib")).map(({ name }) => `${path}/${name}`)
      : [path];
    for (const file of files) if (await align(file)) aligned.push(file);
  }
  return aligned;
}

if (import.meta.main) {
  if (Deno.args.length === 0) throw new Error("tools/align-macho-string-table.js takes directories or libraries");
  console.log(JSON.stringify({ aligned: await alignStringTables(Deno.args) }));
}
