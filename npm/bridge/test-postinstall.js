#!/usr/bin/env node
/**
 * Tests for bridge npm postinstall script.
 * Uses only Node.js built-ins — no external dependencies required.
 *
 * Run: node npm/bridge/test-postinstall.js
 */

'use strict';

const assert = require('assert');
const os = require('os');
const fs = require('fs');
const path = require('path');

const { main, PLATFORM_PACKAGES } = require('./postinstall');

// ---------------------------------------------------------------------------
// Minimal test runner
// ---------------------------------------------------------------------------

let passed = 0;
let failed = 0;

function test(name, fn) {
  try {
    fn();
    console.log(`  ✓ ${name}`);
    passed++;
  } catch (e) {
    console.error(`  ✗ ${name}`);
    console.error(`    ${e.message}`);
    if (process.env.VERBOSE) console.error(e.stack);
    failed++;
  }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const PLATFORM_KEY = `${process.platform}-${process.arch}`;
const BINARY_NAME = process.platform === 'win32' ? 'bridge.exe' : 'bridge';
const SHORT_NAME = (PLATFORM_PACKAGES[PLATFORM_KEY] || '@aptove/bridge-unknown').split('/')[1];

/**
 * Creates a temp directory simulating the node_modules layout:
 *   <tmp>/node_modules/@aptove/bridge/          ← baseDir
 *   <tmp>/node_modules/@aptove/<platform>/bin/  ← sibling binary
 *
 * binaryVersion: if non-null, creates a fake binary that prints "bridge <version>"
 * pkgVersion:    version written into optionalDependencies
 */
function makeLayout({ binaryVersion = null, pkgVersion = '1.2.3' } = {}) {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'bridge-postinstall-test-'));
  const baseDir = path.join(tmp, 'node_modules', '@aptove', 'bridge');
  fs.mkdirSync(baseDir, { recursive: true });

  const packageJsonPath = path.join(baseDir, 'package.json');
  fs.writeFileSync(packageJsonPath, JSON.stringify({
    name: '@aptove/bridge',
    version: pkgVersion,
    optionalDependencies: Object.fromEntries(
      Object.values(PLATFORM_PACKAGES).map(p => [p, pkgVersion])
    ),
  }));

  const binDir = path.join(tmp, 'node_modules', '@aptove', SHORT_NAME, 'bin');

  if (binaryVersion !== null) {
    fs.mkdirSync(binDir, { recursive: true });
    writeFakeBinary(path.join(binDir, BINARY_NAME), binaryVersion);
  }

  return { tmp, baseDir, packageJsonPath, binDir };
}

function writeFakeBinary(binaryPath, version) {
  if (process.platform === 'win32') {
    fs.writeFileSync(binaryPath, `@echo bridge ${version}\r\n`);
  } else {
    fs.writeFileSync(binaryPath, `#!/bin/sh\nprintf 'bridge ${version}'\n`, { mode: 0o755 });
  }
}

/**
 * Runs main() with output captured.
 * exitFn throws a sentinel to stop execution at the first process.exit() call.
 * Returns { exitCode, logs, warns }.
 */
function runMain(opts) {
  const logs = [];
  const warns = [];
  let exitCode = null;

  try {
    main({
      ...opts,
      log:    (...a) => logs.push(a.join(' ')),
      warn:   (...a) => warns.push(a.join(' ')),
      exitFn: (code) => { exitCode = code; throw { __exit: true }; },
    });
  } catch (e) {
    if (!e.__exit) throw e;
  }

  return { exitCode, logs, warns };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

console.log('\npostinstall.js\n');

test('exits 0 cleanly for unsupported platform', () => {
  const { tmp, baseDir, packageJsonPath } = makeLayout();

  const { exitCode, warns } = runMain({
    platformKey: 'freebsd-x64',
    baseDir,
    packageJsonPath,
    installFn: () => { throw new Error('should not call installFn on unsupported platform'); },
  });

  assert.strictEqual(exitCode, 0);
  assert.ok(warns.some(w => w.includes('unsupported platform freebsd-x64')), 'should warn about unsupported platform');

  fs.rmSync(tmp, { recursive: true });
});

test('reports success when binary is present at correct version', () => {
  const { tmp, baseDir, packageJsonPath } = makeLayout({ binaryVersion: '1.2.3', pkgVersion: '1.2.3' });

  const { exitCode, logs } = runMain({
    baseDir,
    packageJsonPath,
    installFn: () => { throw new Error('should not call installFn when version is correct'); },
  });

  assert.strictEqual(exitCode, 0);
  assert.ok(logs.some(l => l.includes('✓') && l.includes('1.2.3')), 'should log success with version');

  fs.rmSync(tmp, { recursive: true });
});

test('installs platform package when binary is missing', () => {
  const { tmp, baseDir, packageJsonPath, binDir } = makeLayout({ binaryVersion: null, pkgVersion: '1.2.3' });

  let installCalledWith = null;
  const { exitCode, logs } = runMain({
    baseDir,
    packageJsonPath,
    installFn: (pkg, ver) => {
      installCalledWith = { pkg, ver };
      // Simulate a successful npm install by creating the binary
      fs.mkdirSync(binDir, { recursive: true });
      writeFakeBinary(path.join(binDir, BINARY_NAME), ver);
    },
  });

  assert.strictEqual(exitCode, 0);
  assert.ok(installCalledWith !== null, 'installFn should have been called');
  assert.strictEqual(installCalledWith.ver, '1.2.3', 'should install expected version');
  assert.ok(logs.some(l => l.includes('✓') && l.includes('1.2.3')), 'should confirm successful install');

  fs.rmSync(tmp, { recursive: true });
});

test('updates platform package when binary version is outdated', () => {
  const { tmp, baseDir, packageJsonPath, binDir } = makeLayout({ binaryVersion: '1.0.0', pkgVersion: '1.2.3' });

  let installCalledWith = null;
  const { exitCode, logs } = runMain({
    baseDir,
    packageJsonPath,
    installFn: (pkg, ver) => {
      installCalledWith = { pkg, ver };
      // Simulate npm updating the binary to the new version
      writeFakeBinary(path.join(binDir, BINARY_NAME), ver);
    },
  });

  assert.strictEqual(exitCode, 0);
  assert.ok(installCalledWith !== null, 'installFn should have been called for update');
  assert.strictEqual(installCalledWith.ver, '1.2.3', 'should update to expected version');
  assert.ok(logs.some(l => l.includes('1.0.0') && l.includes('1.2.3')), 'should log the version transition');
  assert.ok(logs.some(l => l.includes('✓') && l.includes('1.2.3')), 'should confirm successful update');

  fs.rmSync(tmp, { recursive: true });
});

test('exits 0 and warns when install fails (never blocks parent npm install)', () => {
  const { tmp, baseDir, packageJsonPath } = makeLayout({ binaryVersion: null, pkgVersion: '1.2.3' });

  const { exitCode, warns } = runMain({
    baseDir,
    packageJsonPath,
    installFn: () => { throw new Error('network error'); },
  });

  assert.strictEqual(exitCode, 0, 'must exit 0 so the parent `npm install -g` does not fail');
  assert.ok(warns.some(w => w.includes('npm install')), 'should print the manual install command');

  fs.rmSync(tmp, { recursive: true });
});

test('exits 0 and warns when update fails', () => {
  const { tmp, baseDir, packageJsonPath } = makeLayout({ binaryVersion: '1.0.0', pkgVersion: '1.2.3' });

  const { exitCode, warns } = runMain({
    baseDir,
    packageJsonPath,
    installFn: () => { throw new Error('registry down'); },
  });

  assert.strictEqual(exitCode, 0, 'must exit 0 so the parent `npm install -g` does not fail');
  assert.ok(warns.some(w => w.includes('npm install')), 'should print the manual install command');

  fs.rmSync(tmp, { recursive: true });
});

// Skip permission test on Windows — chmod doesn't apply there
if (process.platform !== 'win32') {
  test('fixes execute permissions when binary is present but not executable', () => {
    const { tmp, baseDir, packageJsonPath, binDir } = makeLayout({ pkgVersion: '1.2.3' });

    // Write binary WITHOUT execute bit (simulates GitHub artifact permission stripping)
    fs.mkdirSync(binDir, { recursive: true });
    const binaryPath = path.join(binDir, BINARY_NAME);
    fs.writeFileSync(binaryPath, `#!/bin/sh\nprintf 'bridge 1.2.3'\n`, { mode: 0o644 });

    const { exitCode, logs } = runMain({
      baseDir,
      packageJsonPath,
      installFn: () => { throw new Error('should not call installFn when version is correct'); },
    });

    assert.strictEqual(exitCode, 0);
    assert.ok(logs.some(l => l.includes('✓') && l.includes('1.2.3')), 'should log success after fixing permissions');

    fs.rmSync(tmp, { recursive: true });
  });
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

console.log(`\n${passed} passed, ${failed} failed\n`);

if (failed > 0) process.exit(1);
