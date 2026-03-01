/**
 * bridge postinstall script
 *
 * Checks whether the platform-specific binary package was installed.
 * npm sometimes skips optional dependencies during global installs.
 * If so, prints the exact command to complete installation.
 */

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
let installed = false;

try {
  const packageJsonPath = require.resolve(`${packageName}/package.json`);
  const binaryPath = path.join(path.dirname(packageJsonPath), 'bin', binaryName);
  installed = fs.existsSync(binaryPath);
} catch (e) {
  // package not installed
}

if (installed) {
  console.log(`✓ bridge installed successfully for ${platformKey}`);
} else {
  console.log('');
  console.log(`⚠️  bridge: platform binary not found (npm skipped optional dependency)`);
  console.log(`   To complete installation, run:`);
  console.log(`     npm install -g ${packageName}`);
  console.log('');
}
