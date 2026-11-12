"use strict";

const assert = require("node:assert/strict");
const test = require("node:test");

const { targetTriple } = require("./postinstall.js");

test("maps the release targets whisker publishes", () => {
  assert.equal(targetTriple("darwin", "arm64"), "aarch64-apple-darwin");
  assert.equal(targetTriple("darwin", "x64"), "x86_64-apple-darwin");
  assert.equal(targetTriple("linux", "arm64"), "aarch64-unknown-linux-gnu");
  assert.equal(targetTriple("linux", "x64"), "x86_64-unknown-linux-gnu");
  assert.equal(targetTriple("win32", "x64"), null);
});
