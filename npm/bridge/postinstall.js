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

const platformKey = `${process.platform}-${process.arch}`;
const packageName = PLATFORM_PACKAGES[platformKey];

if (!packageName) {
  console.warn(`⚠️  bridge: unsupported platform ${platformKey}`);
  process.exit(0);
}

const binaryName = process.platform === 'win32' ? 'bridge.exe' : 'bridge';
const shortName = packageName.split('/')[1]; // e.g. "bridge-darwin-arm64"

// postinstall.js lives at @aptove/bridge/postinstall.js
// platform binary lives at @aptove/bridge-darwin-arm64/bin/bridge (sibling package)
const siblingBinaryPath = path.join(__dirname, '..', shortName, 'bin', binaryName);

function isBinaryPresent() {
  return fs.existsSync(siblingBinaryPath);
}

// Returns the version string from `bridge --version` (e.g. "0.1.11"), or null on failure.
function getBinaryVersion() {
  const result = spawnSync(siblingBinaryPath, ['--version'], { stdio: 'pipe' });
  if (result.error || result.status !== 0) return null;
  const output = (result.stdout || '').toString().trim(); // e.g. "bridge 0.1.11"
  const parts = output.split(' ');
  return parts.length >= 2 ? parts[1] : output;
}

function getExpectedVersion() {
  try {
    const pkg = require('./package.json');
    // optionalDependencies values are updated by the release workflow to match the published version
    const deps = pkg.optionalDependencies || {};
    const versions = Object.values(deps).filter(v => v !== '*');
    return versions[0] || pkg.version;
  } catch (e) {
    return null;
  }
}

function installPlatformPackage(version) {
  // During `npm install -g`, npm_config_prefix points to the global prefix.
  // Installing with --prefix ensures the package lands in the same global node_modules tree.
  const prefix = process.env.npm_config_prefix;
  const prefixFlag = prefix ? `--prefix "${prefix}"` : '';
  console.log(`  Installing ${packageName}@${version}...`);
  execSync(
    `npm install ${prefixFlag} --no-save --no-audit --no-fund "${packageName}@${version}"`,
    { stdio: 'inherit' }
  );
}

function tryInstall(version) {
  try {
    installPlatformPackage(version);
    return true;
  } catch (e) {
    return false;
  }
}

// --- Main ---

const expectedVersion = getExpectedVersion();

// Check if binary is present and at the correct version.
if (isBinaryPresent()) {
  const installedVersion = getBinaryVersion();
  if (installedVersion && expectedVersion && installedVersion !== expectedVersion) {
    console.log(`⬆  bridge: updating platform binary ${installedVersion} → ${expectedVersion}...`);
    if (!tryInstall(expectedVersion)) {
      console.warn(`⚠️  bridge: update failed — run: npm install -g ${packageName}@${expectedVersion}`);
      process.exit(0);
    }
  } else {
    const version = installedVersion || getBinaryVersion();
    console.log(`✓ bridge ${version || installedVersion} installed successfully for ${platformKey}`);
    process.exit(0);
  }
} else {
  // Optional dependency was not installed (common with `npm install -g`).
  console.log(`\n⬇  bridge: platform binary not found, installing ${packageName}...`);
  if (!tryInstall(expectedVersion || 'latest')) {
    console.warn(`\n⚠️  bridge: failed to install ${packageName}`);
    console.warn(`   Run manually: npm install -g ${packageName}${expectedVersion ? '@' + expectedVersion : ''}`);
    process.exit(0);
  }
}

// Validate after install using sibling path directly.
// (require.resolve cannot be used here — Node.js caches negative module
// resolution results within the same process.)
if (isBinaryPresent()) {
  const installedVersion = getBinaryVersion();
  if (installedVersion) {
    if (expectedVersion && installedVersion !== expectedVersion) {
      console.warn(`⚠️  bridge: installed ${installedVersion} but expected ${expectedVersion} (may not be published yet)`);
    } else {
      console.log(`✓ bridge ${installedVersion} installed successfully for ${platformKey}`);
    }
  } else {
    console.warn(`⚠️  bridge: binary present but failed to run — try: ${siblingBinaryPath} --version`);
  }
} else {
  console.warn(`\n⚠️  bridge: binary not found after installation`);
  console.warn(`   Run manually: npm install -g ${packageName}${expectedVersion ? '@' + expectedVersion : ''}`);
}
