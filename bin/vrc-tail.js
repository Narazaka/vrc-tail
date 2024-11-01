// @ts-check
const fs = require("fs");
const path = require("path");
const colors = require("colors/safe");
const { program } = require("commander");
const { Tail } = require("tail");

const dir = path.normalize(`${process.env.LOCALAPPDATA}Low\\VRChat\\VRChat`);

program
  .option("-f, --filter <str>", "filter")
  .option("-s, --ignore-blank-lines", "ignoreBlankLines")
  .option("-l, --no-colored-log-level", "noColoredLogLevel")
  .option("-d, --suppress-log-date", "suppressLogDate")
  .parse();

/**
 * @type {{filter?: (line: string) => boolean; ignoreBlankLines: boolean; coloredLogLevel: boolean; suppressLogDate: boolean}}
 */
const options = {
  filter: program.opts().filter,
  ignoreBlankLines: false,
  coloredLogLevel: true,
  suppressLogDate: false,
};

const programOptions = program.opts();
if (programOptions.filter) {
  options.filter = (line) => line.includes(programOptions.filter);
}
if (programOptions.ignoreBlankLines) {
  options.ignoreBlankLines = true;
}
if (programOptions.noColoredLogLevel) {
  options.coloredLogLevel = false;
}
if (programOptions.suppressLogDate) {
  options.suppressLogDate = true;
}

function formatedTimestamp() {
  return formatTimestamp(new Date());
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

const logLevelColors = {
  Log: "blue",
  Warning: "yellow",
  Error: "red",
  Exception: "red",
};

const logLevelRe =
  /^(\d{4}\.\d{2}\.\d{2} \d{2}:\d{2}:\d{2}) (Log|Warning|Error|Exception)/;

const suppressLogDateRe = /^\d{4}\.\d{2}\.\d{2} \d{2}:\d{2}:\d{2} /;

/**
 *
 * @param {string} path
 * @param {number} index
 */
function tail(path, index) {
  new Tail(path, { follow: true }).on("line", (line) => {
    if (options.filter && !options.filter(line)) return;
    if (options.ignoreBlankLines && line.length === 0) return;
    const indexColor = colorNames[index % colorNames.length];
    if (options.coloredLogLevel) {
      const m = logLevelRe.exec(line);
      if (m) {
        const logLevelColor = logLevelColors[m[2]];
        if (logLevelColor) {
          console.log(
            colors[indexColor](`${formatedTimestamp()} [${index}] `) +
              colors[logLevelColor](
                options.suppressLogDate ? m[2] : `${m[1]} ${m[2]}`,
              ) +
              colors[indexColor](line.slice(m[0].length)),
          );
          return;
        }
      }
    }
    if (options.suppressLogDate) {
      console.log(
        colors[indexColor](
          `${formatedTimestamp()} [${index}] ${line.replace(
            suppressLogDateRe,
            "",
          )}`,
        ),
      );
      return;
    }
    console.log(
      colors[indexColor](`${formatedTimestamp()} [${index}] ${line}`),
    );
  });
}

for (let i = 0; i < targetEntries.length; i++) {
  const entry = targetEntries[i];
  tail(entry.path, i);
}

const stdin = process.stdin;
stdin.setRawMode(true);
stdin.resume();
stdin.setEncoding("utf8");
let isFilterInput = false;
let filterInput = "";
stdin.on("data", (data) => {
  const key = /** @type {string} */ (/** @type {unknown} */ (data));
  // ctrl-c ( end of text )
  if (key === "\u0003") {
    process.exit();
  }

  if (isFilterInput) {
    if (key === "\r" || key === "\n") {
      const filter = filterInput;
      options.filter = (line) => line.includes(filter);
      isFilterInput = false;
      filterInput = "";
      process.stdout.write("\n");
      process.stdout.write(`> filter = ${filter}\n`);
      return;
    }
    filterInput += key;
    process.stdout.write(key);
    return;
  }
  switch (key) {
    case "?":
      console.log("> Commands:");
      console.log(">   ? - show this help");
      console.log(">   q - quit");
      console.log(">   s - toggle ignore blank lines");
      console.log(">   l - toggle colored log level");
      console.log(">   d - toggle supress log date");
      console.log(">   /<str> - filter");
      console.log(">   r - reset filter");
      break;
    case "q":
      process.exit(0);
      break;
    case "s":
      options.ignoreBlankLines = !options.ignoreBlankLines;
      process.stdout.write(
        `> ignoreBlankLines = ${options.ignoreBlankLines}\n`,
      );
      break;
    case "l":
      options.coloredLogLevel = !options.coloredLogLevel;
      process.stdout.write(`> coloredLogLevel = ${options.coloredLogLevel}\n`);
      break;
    case "d":
      options.suppressLogDate = !options.suppressLogDate;
      process.stdout.write(`> suppressLogDate = ${options.suppressLogDate}\n`);
      break;
    case "r":
      options.filter = undefined;
      process.stdout.write("> filter cleared!\n");
      break;
    case "/":
      isFilterInput = true;
      filterInput = "";
      process.stdout.write(key);
      break;
  }
});
