// @ts-check

import { sha256sum } from "./sha256sum.mjs";
import fs, { promises as fsp } from "fs";
import { join } from "path";

const basedir = join(import.meta.dirname, "..");

const packageVersion = JSON.parse(
  await fsp.readFile(join(basedir, "package.json"), "utf8"),
).version;
const packageHash = await sha256sum(join(basedir, "vrc-tail.exe"));

function replaceManifestStrings(content) {
  return content
    .replace(/PACKAGE_VERSION/g, packageVersion)
    .replace(/PACKAGE_HASH/g, packageHash);
}

async function replaceManifest(filename) {
  const content = await fsp.readFile(
    join(basedir, "manifest/winget", filename),
    "utf8",
  );
  const replaced = replaceManifestStrings(content);
  await fsp.writeFile(
    join(basedir, "manifest/winget/generated", filename),
    replaced,
  );
}

if (!fs.existsSync(join(basedir, "manifest/winget/generated"))) {
  await fsp.mkdir(join(basedir, "manifest/winget/generated"));
}
await replaceManifest("Narazaka.vrc-tail.yaml");
await replaceManifest("Narazaka.vrc-tail.installer.yaml");
await replaceManifest("Narazaka.vrc-tail.locale.en-US.yaml");
