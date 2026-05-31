#!/usr/bin/env node
'use strict';

const childProcess = require('child_process');
const crypto = require('crypto');
const fs = require('fs');
const https = require('https');
const os = require('os');
const path = require('path');

const REPO = process.env.DBG_REPO || 'https://github.com/zhangsanfenggithub/dbgraph';
const VERSION = process.env.DBG_VERSION || require('../package.json').version;

main().catch((error) => {
  process.stderr.write(`dbgraph: ${error && error.message ? error.message : String(error)}\n`);
  process.exit(1);
});

async function main() {
  const target = detectTarget();
  const binary = await ensureBinary(target);
  const child = childProcess.spawn(binary, process.argv.slice(2), { stdio: 'inherit' });
  child.on('error', (error) => {
    process.stderr.write(`dbgraph: failed to launch binary: ${error.message}\n`);
    process.exit(1);
  });
  child.on('exit', (code, signal) => {
    if (signal) process.kill(process.pid, signal);
    process.exit(code === null ? 1 : code);
  });
}

function detectTarget() {
  const platform = process.platform;
  const arch = process.arch;
  if (platform === 'darwin' && arch === 'x64') return { triple: 'x86_64-apple-darwin', ext: 'tar.gz', exe: 'dbgraph' };
  if (platform === 'darwin' && arch === 'arm64') return { triple: 'aarch64-apple-darwin', ext: 'tar.gz', exe: 'dbgraph' };
  if (platform === 'linux' && arch === 'x64') return { triple: 'x86_64-unknown-linux-gnu', ext: 'tar.gz', exe: 'dbgraph' };
  if (platform === 'linux' && arch === 'arm64') return { triple: 'aarch64-unknown-linux-gnu', ext: 'tar.gz', exe: 'dbgraph' };
  if (platform === 'win32' && arch === 'x64') return { triple: 'x86_64-pc-windows-msvc', ext: 'zip', exe: 'dbgraph.exe' };
  if (platform === 'win32' && arch === 'arm64') return { triple: 'aarch64-pc-windows-msvc', ext: 'zip', exe: 'dbgraph.exe' };
  throw new Error(`unsupported platform ${platform}/${arch}`);
}

async function ensureBinary(target) {
  const tag = normalizeTag(VERSION);
  const cacheRoot = process.env.DBG_CACHE_DIR || defaultCacheDir();
  const installDir = path.join(cacheRoot, `${tag}-${target.triple}`);
  const binary = path.join(installDir, target.exe);
  if (fs.existsSync(binary)) return binary;
  if (process.env.DBG_NO_DOWNLOAD) {
    throw new Error(`cached binary not found at ${binary} and DBG_NO_DOWNLOAD is set`);
  }

  fs.mkdirSync(cacheRoot, { recursive: true });
  const stage = fs.mkdtempSync(path.join(cacheRoot, '.download-'));
  try {
    const asset = `dbgraph-${tag}-${target.triple}.${target.ext}`;
    const base = `${REPO.replace(/\/$/, '')}/releases/download/${tag}`;
    const archive = path.join(stage, asset);
    const checksum = path.join(stage, `${asset}.sha256`);
    await download(`${base}/${asset}`, archive);
    await download(`${base}/${asset}.sha256`, checksum);
    verifyChecksum(archive, checksum);
    fs.mkdirSync(path.join(stage, 'extract'));
    extractArchive(archive, path.join(stage, 'extract'), target.ext);
    const found = findBinary(path.join(stage, 'extract'), target.exe);
    if (!found) throw new Error(`downloaded archive did not contain ${target.exe}`);
    fs.mkdirSync(installDir, { recursive: true });
    fs.copyFileSync(found, binary);
    if (process.platform !== 'win32') fs.chmodSync(binary, 0o755);
  } finally {
    fs.rmSync(stage, { recursive: true, force: true });
  }
  return binary;
}

function normalizeTag(version) {
  if (version === 'latest' || version.startsWith('v')) return version;
  return `v${version}`;
}

function defaultCacheDir() {
  if (process.platform === 'win32' && process.env.LOCALAPPDATA) {
    return path.join(process.env.LOCALAPPDATA, 'dbgraph', 'npm-cache');
  }
  return path.join(os.homedir(), '.cache', 'dbgraph');
}

function download(url, dest) {
  return new Promise((resolve, reject) => {
    const request = https.get(url, { headers: { 'User-Agent': 'dbgraph-npm-wrapper' } }, (response) => {
      if (response.statusCode >= 300 && response.statusCode < 400 && response.headers.location) {
        response.resume();
        download(new URL(response.headers.location, url).toString(), dest).then(resolve, reject);
        return;
      }
      if (response.statusCode !== 200) {
        response.resume();
        reject(new Error(`download failed with HTTP ${response.statusCode}: ${url}`));
        return;
      }
      const file = fs.createWriteStream(dest);
      response.pipe(file);
      file.on('finish', () => file.close(resolve));
      file.on('error', reject);
    });
    request.on('error', reject);
  });
}

function verifyChecksum(archive, checksumFile) {
  const expected = fs.readFileSync(checksumFile, 'utf8').trim().split(/\s+/)[0].toLowerCase();
  const actual = crypto.createHash('sha256').update(fs.readFileSync(archive)).digest('hex');
  if (expected !== actual) {
    throw new Error(`checksum mismatch for ${path.basename(archive)}`);
  }
}

function extractArchive(archive, dest, ext) {
  const args = ext === 'zip'
    ? ['-xf', archive, '-C', dest]
    : ['-xzf', archive, '-C', dest];
  const result = childProcess.spawnSync('tar', args, { stdio: 'ignore' });
  if (result.error) throw new Error(`tar unavailable: ${result.error.message}`);
  if (result.status !== 0) throw new Error(`tar failed with exit code ${result.status}`);
}

function findBinary(root, exe) {
  const entries = fs.readdirSync(root, { withFileTypes: true });
  for (const entry of entries) {
    const full = path.join(root, entry.name);
    if (entry.isFile() && entry.name === exe) return full;
    if (entry.isDirectory()) {
      const nested = findBinary(full, exe);
      if (nested) return nested;
    }
  }
  return null;
}
