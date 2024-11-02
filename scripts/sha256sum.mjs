// @ts-check

import { createHash } from "crypto";
import { createReadStream } from "fs";

/**
 *
 * @param {string} filepath
 * @returns
 */
export function sha256sum(filepath) {
  return new Promise(function (resolve, reject) {
    const hash = createHash("sha256");
    const input = createReadStream(filepath);
    input.on("data", function (data) {
      hash.update(data);
    });
    input.on("end", function () {
      resolve(hash.digest("hex").toUpperCase());
    });
    input.on("error", reject);
    input.resume();
  });
}
