#!/usr/bin/env node
"use strict";

const { spawn } = require("child_process");
const fs = require("fs");
const path = require("path");

const bin = path.join(__dirname, "..", "vendor", "whisker");
if (!fs.existsSync(bin)) {
  console.error(
    "whisker binary is missing. Reinstall with: npm install -g @bugeng777/whisker@latest",
  );
  process.exit(1);
}

const child = spawn(bin, process.argv.slice(2), { stdio: "inherit" });
child.on("error", (err) => {
  console.error(err.message);
  process.exit(1);
});
child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code === null ? 1 : code);
});
