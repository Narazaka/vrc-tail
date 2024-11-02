// @ts-check

import { sha256sum } from "./sha256sum.mjs";
import fs from "fs/promises";
import { join } from "path";

const basedir = join(import.meta.dirname, "..");

const packageVersion = JSON.parse(
  await fs.readFile(join(basedir, "package.json"), "utf8"),
).version;
const packageHash = await sha256sum(join(basedir, "vrc-tail.exe"));

function replaceManifestStrings(content) {
  return content
    .replace(/PACKAGE_VERSION/g, packageVersion)
    .replace(/PACKAGE_HASH/g, packageHash);
}

async function replaceManifest(filename) {
  const content = await fs.readFile(
    join(basedir, "manifest/winget", filename),
    "utf8",
  );
  const replaced = replaceManifestStrings(content);
  await fs.writeFile(
    join(basedir, "manifest/winget/generated", filename),
    replaced,
  );
}

await replaceManifest("Narazaka.vrc-tail.yaml");
await replaceManifest("Narazaka.vrc-tail.installer.yaml");
await replaceManifest("Narazaka.vrc-tail.locale.en-US.yaml");
