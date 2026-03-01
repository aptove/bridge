/**
 * bridge postinstall script
 *
 * Ensures the platform-specific binary package is installed at the correct version.
 * npm sometimes skips optional dependencies during global installs, so we detect
 * this and install the platform package explicitly if needed.
 * Validates the final binary by running it with --version.
 */

const { execSync, spawnSync } = require('child_process');
const path = require('path');
const fs = require('fs');

const PLATFORM_PACKAGES = {
  'darwin-arm64': '@aptove/bridge-darwin-arm64',
  'darwin-x64':   '@aptove/bridge-darwin-x64',
  'linux-arm64':  '@aptove/bridge-linux-arm64',
  'linux-x64':    '@aptove/bridge-linux-x64',
  'win32-x64':    '@aptove/bridge-win32-x64',
};

function isBinaryPresent(binaryPath) {
  return fs.existsSync(binaryPath);
}

// Returns the version string from `bridge --version` (e.g. "0.1.12"), or null on failure.
function getBinaryVersion(binaryPath) {
  const result = spawnSync(binaryPath, ['--version'], { stdio: 'pipe' });
  if (result.error || result.status !== 0) return null;
  const output = (result.stdout || '').toString().trim(); // e.g. "bridge 0.1.12"
  const parts = output.split(' ');
  return parts.length >= 2 ? parts[1] : output;
}

// Reads the expected platform package version from the main package's optionalDependencies.
function getExpectedVersion(packageJsonPath) {
  try {
    delete require.cache[require.resolve(packageJsonPath)];
    const pkg = require(packageJsonPath);
    // optionalDependencies values are updated by the release workflow to match the published version
    const deps = pkg.optionalDependencies || {};
    const versions = Object.values(deps).filter(v => v !== '*');
    return versions[0] || pkg.version;
  } catch (e) {
    return null;
  }
}

/**
 * Main postinstall logic.
 *
 * All I/O is injectable for testing:
 *   baseDir        — replaces __dirname for computing the sibling binary path
 *   packageJsonPath— path to the main package's package.json
 *   platformKey    — override platform detection (default: process.platform-process.arch)
 *   npmPrefix      — override npm_config_prefix
 *   installFn      — (packageName, version) => void; throws on failure
 *                    defaults to running `npm install` via execSync
 *   log / warn     — console.log / console.warn replacements
 *   exitFn         — process.exit replacement
 */
function main({
  baseDir = __dirname,
  packageJsonPath = path.join(__dirname, 'package.json'),
  platformKey = `${process.platform}-${process.arch}`,
  npmPrefix = process.env.npm_config_prefix,
  installFn = null,
  log = (...a) => console.log(...a),
  warn = (...a) => console.warn(...a),
  exitFn = process.exit,
} = {}) {
  const packageName = PLATFORM_PACKAGES[platformKey];

  if (!packageName) {
    warn(`⚠️  bridge: unsupported platform ${platformKey}`);
    exitFn(0); return;
  }

  const binaryName = process.platform === 'win32' ? 'bridge.exe' : 'bridge';
  const shortName = packageName.split('/')[1]; // e.g. "bridge-darwin-arm64"

  // postinstall.js lives at @aptove/bridge/postinstall.js
  // platform binary lives at @aptove/bridge-darwin-arm64/bin/bridge (sibling package)
  const siblingBinaryPath = path.join(baseDir, '..', shortName, 'bin', binaryName);

  function tryInstall(version) {
    const fn = installFn || ((pkg, ver) => {
      const prefixFlag = npmPrefix ? `--prefix "${npmPrefix}"` : '';
      execSync(
        `npm install ${prefixFlag} --no-save --no-audit --no-fund "${pkg}@${ver}"`,
        { stdio: 'inherit' }
      );
    });
    try {
      log(`  Installing ${packageName}@${version}...`);
      fn(packageName, version);
      return true;
    } catch (e) {
      return false;
    }
  }

  const expectedVersion = getExpectedVersion(packageJsonPath);

  if (isBinaryPresent(siblingBinaryPath)) {
    const installedVersion = getBinaryVersion(siblingBinaryPath);
    if (installedVersion && expectedVersion && installedVersion !== expectedVersion) {
      log(`⬆  bridge: updating platform binary ${installedVersion} → ${expectedVersion}...`);
      if (!tryInstall(expectedVersion)) {
        warn(`⚠️  bridge: update failed — run: npm install -g ${packageName}@${expectedVersion}`);
        exitFn(0); return;
      }
    } else {
      log(`✓ bridge ${installedVersion || '(unknown)'} installed successfully for ${platformKey}`);
      exitFn(0); return;
    }
  } else {
    log(`\n⬇  bridge: platform binary not found, installing ${packageName}...`);
    if (!tryInstall(expectedVersion || 'latest')) {
      warn(`\n⚠️  bridge: failed to install ${packageName}`);
      warn(`   Run manually: npm install -g ${packageName}${expectedVersion ? '@' + expectedVersion : ''}`);
      exitFn(0); return;
    }
  }

  // Validate after install using sibling path directly.
  // (require.resolve cannot be used here — Node.js caches negative module
  // resolution results within the same process.)
  if (isBinaryPresent(siblingBinaryPath)) {
    const installedVersion = getBinaryVersion(siblingBinaryPath);
    if (installedVersion) {
      if (expectedVersion && installedVersion !== expectedVersion) {
        warn(`⚠️  bridge: installed ${installedVersion} but expected ${expectedVersion} (may not be published yet)`);
      } else {
        log(`✓ bridge ${installedVersion} installed successfully for ${platformKey}`);
      }
    } else {
      warn(`⚠️  bridge: binary present but failed to run — try: ${siblingBinaryPath} --version`);
    }
  } else {
    warn(`\n⚠️  bridge: binary not found after installation`);
    warn(`   Run manually: npm install -g ${packageName}${expectedVersion ? '@' + expectedVersion : ''}`);
  }

  exitFn(0);
}

module.exports = { main, PLATFORM_PACKAGES, isBinaryPresent, getBinaryVersion, getExpectedVersion };

if (require.main === module) {
  main();
}
