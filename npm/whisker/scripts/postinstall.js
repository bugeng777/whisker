"use strict";

const crypto = require("crypto");
const { execFileSync } = require("child_process");
const fs = require("fs");
const https = require("https");
const os = require("os");
const path = require("path");

const ROOT = path.join(__dirname, "..");
const CHECKSUMS_PATH = path.join(ROOT, "checksums.json");
const VENDOR = path.join(ROOT, "vendor");

const TARGETS = {
  "darwin:arm64": "aarch64-apple-darwin",
  "darwin:x64": "x86_64-apple-darwin",
  "linux:arm64": "aarch64-unknown-linux-gnu",
  "linux:x64": "x86_64-unknown-linux-gnu",
};

function targetTriple(platform, arch) {
  return TARGETS[`${platform}:${arch}`] || null;
}

function download(url, redirects = 0) {
  return new Promise((resolve, reject) => {
    if (redirects > 5) {
      reject(new Error(`too many redirects for ${url}`));
      return;
    }
    const request = https.get(
      url,
      { headers: { "user-agent": "whisker-npm-install" } },
      (response) => {
        const status = response.statusCode || 0;
        if (status >= 300 && status < 400 && response.headers.location) {
          response.resume();
          resolve(download(response.headers.location, redirects + 1));
          return;
        }
        if (status !== 200) {
          response.resume();
          reject(new Error(`download failed: ${url} (${status})`));
          return;
        }
        const chunks = [];
        response.on("data", (chunk) => chunks.push(chunk));
        response.on("end", () => resolve(Buffer.concat(chunks)));
        response.on("error", reject);
      },
    );
    request.on("error", reject);
  });
}

async function install() {
  const spec = JSON.parse(fs.readFileSync(CHECKSUMS_PATH, "utf8"));
  const triple = targetTriple(process.platform, process.arch);
  if (!triple) {
    throw new Error(
      `unsupported platform: ${process.platform} ${process.arch} (macOS and Linux, x64 and arm64 only)`,
    );
  }
  const expected = spec.targets && spec.targets[triple];
  if (!expected) {
    throw new Error(`checksum missing for ${triple}`);
  }
  const version = String(spec.version).replace(/^v/, "");
  const installed = path.join(VENDOR, "VERSION");
  const dest = path.join(VENDOR, "whisker");
  if (
    fs.existsSync(dest) &&
    fs.existsSync(installed) &&
    fs.readFileSync(installed, "utf8").trim() === version
  ) {
    return;
  }

  const asset = `whisker-${triple}.tar.gz`;
  const url = `https://github.com/bugeng777/whisker/releases/download/v${version}/${asset}`;
  const archive = await download(url);
  const actual = crypto.createHash("sha256").update(archive).digest("hex");
  if (actual !== expected) {
    throw new Error(`checksum mismatch for ${asset}`);
  }

  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "whisker-npm-"));
  try {
    const archivePath = path.join(tmp, asset);
    fs.writeFileSync(archivePath, archive);
    execFileSync("tar", ["-xzf", archivePath, "-C", tmp], { stdio: "inherit" });
    const extracted = path.join(tmp, "whisker");
    if (!fs.existsSync(extracted)) {
      throw new Error("release archive does not contain a whisker binary");
    }
    fs.mkdirSync(VENDOR, { recursive: true });
    const staged = path.join(VENDOR, `.whisker.${process.pid}`);
    fs.copyFileSync(extracted, staged);
    fs.chmodSync(staged, 0o755);
    fs.renameSync(staged, dest);
    fs.writeFileSync(installed, `${version}\n`);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
  console.log(`installed whisker ${version}`);
}

if (require.main === module) {
  install().catch((err) => {
    console.error(err.message || err);
    process.exit(1);
  });
}

module.exports = { targetTriple };
