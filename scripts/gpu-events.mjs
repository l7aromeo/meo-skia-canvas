#!/usr/bin/env node
// What the GPU did during a window of time, as macOS recorded it: GPU restart
// reports, and Metal command buffers that failed.
//
// A test that reads back blank or stale pixels with no error is what a GPU
// recovery looks like from inside this library -- macOS discards every other
// process's work in flight, and Skia's readback does not notice. Whether one
// happened cannot be reconstructed after the fact from the test's own output,
// so this reads what the system kept:
//
// - /Library/Logs/DiagnosticReports/gpuEvent-<process>-<time>.ips, and the same
//   under ~/Library, which macOS writes when the GPU restarts. A report names
//   the process the restart was charged to and, inside, why.
// - The unified log, for a Metal command buffer that ended in error -- the
//   message Skia logs for one it waited on, and Metal's own error domain.
//
// It only reads. It never needs sudo, and changes no setting: both sources are
// readable by the logged-in user as macOS ships them.
//
//   node scripts/gpu-events.mjs --since <time> [--until <time>]
//   node scripts/gpu-events.mjs --run -- <command> [args...]
//
// A time is anything `Date` parses, or `HH:MM[:SS]` meaning today. `--run`
// runs the command and, only if it fails, reports the window from its start
// to its end, then exits with the command's own status -- which is how the
// test recipes use it. On anything but macOS it says there is nothing to read
// and exits 0, so wrapping a command with it is safe everywhere.

import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

/** Where macOS files its diagnostic reports, system-wide and per user. */
const REPORT_DIRS = [
  "/Library/Logs/DiagnosticReports",
  join(homedir(), "Library/Logs/DiagnosticReports"),
];

/** A GPU restart report: `gpuEvent-<process>-YYYY-MM-DD-HHMMSS.ips`. */
const REPORT_NAME =
  /^gpuEvent-(.+)-(\d{4})-(\d{2})-(\d{2})-(\d{2})(\d{2})(\d{2})\.ips$/;

/**
 * The unified-log messages that mean a Metal command buffer failed: Skia's own
 * line for a buffer it waited on, and Metal's error, in its domain name and in
 * the IOGPU reason it carries.
 */
export const LOG_PREDICATE =
  'eventMessage CONTAINS "MTLCommandBufferErrorDomain" OR ' +
  'eventMessage CONTAINS "Error submitting command buffer" OR ' +
  'eventMessage CONTAINS[c] "kIOGPUCommandBuffer"';

/** How many log lines to print before summarising the rest. */
const LOG_LINES_SHOWN = 20;

/** The time a report's name records, read as local time, as macOS writes it. */
export function reportTime(name) {
  const m = REPORT_NAME.exec(name);
  if (!m) return null;
  const [, process, y, mo, d, h, mi, s] = m;
  return { process, at: new Date(+y, +mo - 1, +d, +h, +mi, +s) };
}

/** `HH:MM[:SS]` as today, or anything `Date` reads. */
export function parseTime(text) {
  const clock = /^(\d{1,2}):(\d{2})(?::(\d{2}))?$/.exec(text);
  if (clock) {
    const t = new Date();
    t.setHours(+clock[1], +clock[2], +(clock[3] ?? 0), 0);
    return t;
  }
  const t = new Date(text);
  if (Number.isNaN(+t))
    throw Error(`gpu-events: cannot read a time from ${JSON.stringify(text)}`);
  return t;
}

/** `YYYY-MM-DD HH:MM:SS` in local time, the form `log show` takes. */
const local = (t) => {
  const p = (n) => String(n).padStart(2, "0");
  return `${t.getFullYear()}-${p(t.getMonth() + 1)}-${p(t.getDate())} ${p(t.getHours())}:${p(t.getMinutes())}:${p(t.getSeconds())}`;
};

/**
 * Why a restart report says the GPU restarted, if it can be read: a report is
 * one line of JSON header, then a JSON body whose `analysis` names the reason.
 */
function restartReason(path) {
  try {
    const text = readFileSync(path, "utf8");
    return (
      JSON.parse(text.slice(text.indexOf("\n") + 1))?.analysis
        ?.restart_reason_desc ?? null
    );
  } catch {
    return null;
  }
}

/** The GPU restart reports filed between `since` and `until`. */
export function restartReports(since, until, dirs = REPORT_DIRS) {
  return dirs
    .filter((dir) => existsSync(dir))
    .flatMap((dir) => {
      let names;
      try {
        names = readdirSync(dir);
      } catch {
        return [];
      }
      return names.flatMap((name) => {
        const parsed = reportTime(name);
        if (!parsed) return [];
        // The name's time is to the second; the window's ends are not, so
        // the start is widened to its second as well.
        const floor = new Date(since);
        floor.setMilliseconds(0);
        if (parsed.at < floor || parsed.at > until) return [];
        return [
          {
            ...parsed,
            path: join(dir, name),
            reason: restartReason(join(dir, name)),
          },
        ];
      });
    })
    .sort((a, b) => a.at - b.at);
}

/** Lines the unified log holds for a failed command buffer in the window. */
export function commandBufferErrors(since, until) {
  const out = execFileSync(
    "/usr/bin/log",
    [
      "show",
      "--style",
      "compact",
      "--start",
      local(since),
      "--end",
      local(until),
      "--predicate",
      LOG_PREDICATE,
    ],
    { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 },
  );
  // The first line is the column header.
  return out
    .split("\n")
    .filter((line) => line && !line.startsWith("Timestamp"));
}

/** Prints what macOS recorded about the GPU between `since` and `until`. */
export function report(since, until, print = console.log) {
  if (process.platform !== "darwin") {
    print(
      "gpu-events: GPU restart reports are read on macOS only; nothing to read here",
    );
    return;
  }
  const reports = restartReports(since, until);
  let errors;
  try {
    errors = commandBufferErrors(since, until);
  } catch (e) {
    errors = null;
    print(
      `gpu-events: the unified log could not be read (${e.message.split("\n")[0]})`,
    );
  }
  print(`gpu-events: ${local(since)} to ${local(until)}`);
  print(`  ${reports.length} GPU restart report(s)`);
  for (const r of reports)
    print(
      `    ${local(r.at)}  ${r.process}${r.reason ? `  (${r.reason})` : ""}  ${r.path}`,
    );
  if (errors) {
    print(
      `  ${errors.length} failed Metal command buffer line(s) in the unified log`,
    );
    for (const line of errors.slice(0, LOG_LINES_SHOWN))
      print(`    ${line.slice(0, 220)}`);
    if (errors.length > LOG_LINES_SHOWN)
      print(`    ... and ${errors.length - LOG_LINES_SHOWN} more`);
  }
  if (!reports.length && errors && !errors.length)
    print(
      "  nothing: no GPU restart and no failed command buffer was recorded in this window",
    );
}

/** Runs `command`, reports the window only if it fails, and returns its status. */
export function run(command, args) {
  const since = new Date();
  const { status, error } = spawnSync(command, args, { stdio: "inherit" });
  if (error) throw error;
  // A signal death leaves `status` null, which must not read as success.
  const code = status ?? 1;
  if (code !== 0) report(since, new Date());
  return code;
}

function selfTest() {
  const cases = [
    ["gpuEvent-node-2026-10-22-181523.ips", "node", "2026-10-22T18:15:23"],
    [
      "gpuEvent-cameracaptured-2026-10-22-181013.ips",
      "cameracaptured",
      "2026-10-22T18:10:13",
    ],
    [
      "gpuEvent-com.apple.WebKit.GPU-2026-01-02-030405.ips",
      "com.apple.WebKit.GPU",
      "2026-01-02T03:04:05",
    ],
  ];
  const refused = [
    "hanger-2026-10-22-180614.ips",
    "gpuEvent-node.ips",
    "JetsamEvent-2026-10-22-170951.ips",
  ];
  const wrong = [
    ...cases
      .filter(([name, proc, at]) => {
        const r = reportTime(name);
        return !r || r.process !== proc || +r.at !== +new Date(at);
      })
      .map(([name]) => name),
    ...refused.filter((name) => reportTime(name) !== null),
  ];
  if (+parseTime("18:15") !== +new Date(new Date().setHours(18, 15, 0, 0)))
    wrong.push("parseTime 18:15");
  if (wrong.length) {
    console.error(`gpu-events self-test failed: ${wrong.join(", ")}`);
    process.exit(1);
  }
  console.log(
    `gpu-events self-test: ${cases.length} report names read, ${refused.length} other names refused`,
  );
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const argv = process.argv.slice(2);
  if (argv[0] === "--self-test") selfTest();
  else if (argv[0] === "--run") {
    const at = argv.indexOf("--");
    const [command, ...args] = at < 0 ? argv.slice(1) : argv.slice(at + 1);
    if (!command) throw Error("gpu-events: --run needs a command after --");
    process.exit(run(command, args));
  } else {
    const value = (flag) => {
      const at = argv.indexOf(flag);
      return at < 0 ? undefined : argv[at + 1];
    };
    const since = value("--since");
    if (!since)
      throw Error(
        "gpu-events: usage: --since <time> [--until <time>] | --run -- <command>",
      );
    const until = value("--until");
    report(parseTime(since), until ? parseTime(until) : new Date());
  }
}
