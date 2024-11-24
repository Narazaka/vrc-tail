// @ts-check
const fs = require("fs");
const path = require("path");
const colors = require("colors/safe");
const { program } = require("commander");
const { Tail } = require("tail");
const { watch } = require("chokidar");
const { escapeRegExp } = require("es-toolkit/string");
const { version } = require("../package.json");

const dir = path.normalize(`${process.env.LOCALAPPDATA}Low\\VRChat\\VRChat`);

program
  .version(version)
  .option("-f, --filter <str>", "filter")
  .option("-c, --case-sensitive", "case sensitive")
  .option("-s, --ignore-blank-lines", "ignore blank lines")
  .option("-L, --no-colored-log-level", "no colored log level")
  .option("-d, --suppress-log-date", "suppress log date")
  .option("-g, --group-period <sec>", "log group pediod (seconds)")
  .option("--no-watch", "noWatch")
  .parse();

/**
 * @type {{filter?: (line: string) => boolean; caseSensitive: boolean; ignoreBlankLines: boolean; coloredLogLevel: boolean; suppressLogDate: boolean; watch: boolean; groupPediod: number}}
 */
const options = {
  filter: program.opts().filter,
  caseSensitive: false,
  ignoreBlankLines: false,
  coloredLogLevel: true,
  suppressLogDate: false,
  watch: true,
  groupPediod: 30,
};

const programOptions = program.opts();
if (programOptions.filter) {
  options.filter = (line) => line.includes(programOptions.filter);
}
if (programOptions.caseSensitive) {
  options.caseSensitive = true;
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
if (programOptions.noWatch) {
  options.watch = false;
}
if (programOptions.groupPediod) {
  options.groupPediod = Number(programOptions.groupPediod);
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

const entries = [];
const targetEntries = [];
/** @type {Tail[]} */
const tails = [];

/**
 * @param {string} filepath
 */
function addLogFile(filepath) {
  const filename = path.basename(filepath);
  if (!targetRe.test(filename)) return;
  const entry = {
    path: filepath,
    time: dateOf(filename)?.getTime() ?? 0,
  };
  const index = entries.findIndex((e) => e.time > entry.time);
  if (index === -1) {
    entries.push(entry);
  } else {
    entries.splice(index, 0, entry);
  }
}

function setTails() {
  for (let i = 0; i < entries.length; i++) {
    const entry = entries[i];
    const previousEntry = targetEntries[targetEntries.length - 1];
    if (!previousEntry) {
      targetEntries.push(entry);
      continue;
    }
    if (entry.time - previousEntry.time > 1000 * options.groupPediod) {
      targetEntries.length = 0;
      if (tails.length > 0) {
        for (const tail of tails) {
          tail.unwatch();
        }
        tails.length = 0;
        console.log("-".repeat(79));
      }
    }
    targetEntries.push(entry);
  }

  for (let i = tails.length; i < targetEntries.length; i++) {
    const entry = targetEntries[i];
    tails.push(tail(entry.path, i));
  }
}

/**
 * @param {string} filepath
 */
function addLogFileAndSetTails(filepath) {
  addLogFile(filepath);
  setTails();
}

if (!fs.existsSync(dir)) {
  console.error(`Log directory [${dir}] not found`);
  process.exit(1);
}

for (const filename of fs.readdirSync(dir)) {
  addLogFile(path.join(dir, filename));
}
setTails();
if (options.watch) {
  watch(dir, { depth: 0, ignoreInitial: true }).on(
    "add",
    addLogFileAndSetTails,
  );
} else {
  if (entries.length === 0) {
    console.error("No log files found");
    process.exit(1);
  }
}

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
  const tail = new Tail(path, { follow: true });
  tail.on("line", (line) => {
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
  return tail;
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
      const ignoreCaseFilter = new RegExp(escapeRegExp(filter), "i");
      options.filter = (line) =>
        options.caseSensitive
          ? line.includes(filter)
          : ignoreCaseFilter.test(line);
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
      console.log(">   c - toggle case sensitive");
      console.log(">   s - toggle ignore blank lines");
      console.log(">   l - toggle colored log level");
      console.log(">   d - toggle supress log date");
      console.log(">   /<str> - filter");
      console.log(">   r - reset filter");
      break;
    case "\r":
      process.stdout.write("\n");
      break;
    case "\n":
      process.stdout.write("\n");
      break;
    case "q":
      process.exit(0);
      break;
    case "c":
      options.caseSensitive = !options.caseSensitive;
      process.stdout.write(`> caseSensitive = ${options.caseSensitive}\n`);
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
