// @ts-check

"use strict";

// The addon starts threads that live for the process -- rayon's pool, the GIF
// encoder's palette pool -- and parks them in its own code. Node unloads an
// addon when the last environment that loaded it is torn down, a worker's
// included, and on Windows that unmaps the DLL under those threads: one that
// wakes afterwards executes freed memory. So the addon pins itself as it
// loads, and outlives the last worker that used it.
//
// A second process, because this one has loaded the addon on its main thread
// already, which would keep it mapped whatever the addon does.

const { execFileSync } = require("child_process"),
  { assert, describe, test } = require("../runner");

/** Long enough for the worker's environment to be torn down after `exit`. */
const AFTER_EXIT_MS = 500;

const why =
  process.platform !== "win32"
    ? "skipped: only Windows unmaps an addon its last worker leaves"
    : null;

describe("Unload", () => {
  test(
    "the addon stays loaded after the last worker that loaded it ends",
    { skip: why ?? false },
    () => {
      // The main thread never loads the addon; a worker loads it, encodes a
      // GIF, which starts the palette pool, and ends. The library's path
      // reaches the worker as data -- an argument, then `workerData` --
      // never spliced into the code: written into a template literal, a
      // Windows path's backslashes are read as escapes, and `D:\a\...`
      // arrives as `D:a...`, which no module answers to.
      const script = `
        const { Worker } = require("worker_threads");
        const worker = new Worker(\`
          const { workerData } = require("worker_threads");
          const { Canvas } = require(workerData);
          const canvas = new Canvas(8, 8);
          canvas.gpu = false;
          const ctx = canvas.getContext("2d");
          ctx.fillStyle = "red";
          ctx.fillRect(0, 0, 8, 8);
          canvas.toBufferSync("gif");
        \`, { eval: true, workerData: process.argv[1] });
        worker.on("exit", () => setTimeout(() => {
          const loaded = process.report.getReport().sharedObjects
            .filter((path) => /\\.node$/i.test(path));
          console.log(JSON.stringify(loaded));
        }, ${AFTER_EXIT_MS}));
      `;
      const loaded = JSON.parse(
        execFileSync(
          process.execPath,
          ["-e", script, require.resolve("../../lib")],
          {
            encoding: "utf8",
            env: process.env,
          },
        ),
      );
      assert.equal(
        loaded.length,
        1,
        `addons mapped after the worker ended: ${JSON.stringify(loaded)}`,
      );
    },
  );
});
