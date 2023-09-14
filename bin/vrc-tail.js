// @ts-check
const fs = require("fs");
const path = require("path");
const readline = require("node:readline");
const colors = require("colors/safe");
const { program } = require("commander");
const { Tail } = require("tail");

const dir = path.normalize(`${process.env.LOCALAPPDATA}Low\\VRChat\\VRChat`);

program.option("-f, --filter <str>", "filter").parse();

/**
 * @type {{filter?: (line: string) => boolean}}
 */
const options = {
  filter: program.opts().filter,
};

const programOptions = program.opts();
if (programOptions.filter) {
  options.filter = (line) => line.includes(programOptions.filter);
}

/**
 *
 * @param {Date} date
 */
function formatTimestamp(date) {
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(
    date.getDate(),
  )} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(
    date.getSeconds(),
  )}.${pad(date.getMilliseconds(), 4)}`;
}

/**
 *
 * @param {number} n
 * @param {number} width
 * @returns
 */
function pad(n, width = 2) {
  return n.toString().padStart(width, "0");
}

const targetRe = /^output_log_(\d+)-(\d+)-(\d+)_(\d+)-(\d+)-(\d+)\.txt$/;
/**
 *
 * @param {string} str
 */
function dateOf(str) {
  const m = targetRe.exec(str);
  if (!m) return null;
  return new Date(
    Number(m[1]),
    Number(m[2]) - 1,
    Number(m[3]),
    Number(m[4]),
    Number(m[5]),
    Number(m[6]),
  );
}

const entries = fs
  .readdirSync(dir)
  .filter((f) => targetRe.test(f))
  .map((f) => ({
    name: f,
    path: path.join(dir, f),
    time: dateOf(f)?.getTime() ?? 0,
  }))
  .sort((a, b) => b.time - a.time);
if (entries.length === 0) {
  console.error("No log files found");
  process.exit(1);
}

const targetEntries = [];

for (const entry of entries) {
  const previousEntry = targetEntries[targetEntries.length - 1];
  if (!previousEntry) {
    targetEntries.push(entry);
    continue;
  }
  if (previousEntry.time - entry.time > 1000 * 30) {
    // 30 seconds
    break;
  }
  targetEntries.push(entry);
}

targetEntries.reverse();

const colorNames = [
  // "red",
  "green",
  // "yellow",
  "blue",
  "magenta",
  "cyan",
  "white",
  "gray",
];

/**
 *
 * @param {string} path
 * @param {number} index
 */
function tail(path, index) {
  new Tail(path, { follow: true }).on("line", (line) => {
    if (options.filter && !options.filter(line)) return;
    console.log(
      colors[colorNames[index % colorNames.length]](
        `${formatTimestamp(new Date())} [${index}] ${line}`,
      ),
    );
  });
}

for (let i = 0; i < targetEntries.length; i++) {
  const entry = targetEntries[i];
  tail(entry.path, i);
}

const rl = readline.createInterface({
  input: process.stdin,
});

rl.on("line", (line) => {
  switch (line) {
    case "?":
      console.log("Commands:");
      console.log("  ? - show this help");
      console.log("  q - quit");
      console.log("  / <str> - filter");
      console.log("  r - reset filter");
      break;
    case "q":
      process.exit(0);
      break;
    case "r":
      options.filter = undefined;
      break;
    default:
      if (line.startsWith("/")) {
        const filter = line.slice(1).trim();
        options.filter = (line) => line.includes(filter);
      }
      break;
  }
});
