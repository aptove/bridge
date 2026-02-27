/**
 * bridge postinstall script
 *
 * Verifies the correct platform-specific package was installed
 * and the binary is executable.
 */

const { execSync } = require('child_process');
const path = require('path');
const fs = require('fs');

const PLATFORM_PACKAGES = {
  'darwin-arm64': '@aptove/bridge-darwin-arm64',
  'darwin-x64':   '@aptove/bridge-darwin-x64',
  'linux-arm64':  '@aptove/bridge-linux-arm64',
  'linux-x64':    '@aptove/bridge-linux-x64',
  'win32-x64':    '@aptove/bridge-win32-x64',
};

function main() {
  const platformKey = `${process.platform}-${process.arch}`;
  const packageName = PLATFORM_PACKAGES[platformKey];

  if (!packageName) {
    console.warn(`⚠️  bridge: Unsupported platform ${platformKey}`);
    console.warn('   Supported platforms: darwin-arm64, darwin-x64, linux-arm64, linux-x64, win32-x64');
    return;
  }

  try {
    const packagePath = require.resolve(`${packageName}/package.json`);
    const binaryName = process.platform === 'win32' ? 'bridge.exe' : 'bridge';
    const binaryPath = path.join(path.dirname(packagePath), 'bin', binaryName);

    if (!fs.existsSync(binaryPath)) {
      console.warn(`⚠️  bridge: Binary not found at ${binaryPath}`);
      return;
    }

    if (process.platform !== 'win32') {
      try {
        fs.chmodSync(binaryPath, 0o755);
      } catch (e) {
        // Not critical
      }
    }

    try {
      execSync(`"${binaryPath}" --version`, { stdio: 'pipe' });
      console.log(`✓ bridge installed successfully for ${platformKey}`);
    } catch (e) {
      console.warn(`⚠️  bridge: Binary exists but failed to execute on ${platformKey}`);
    }
  } catch (e) {
    console.warn(`⚠️  bridge: Platform package ${packageName} not installed`);
    console.warn('   This is expected on CI or unsupported platforms');
  }
}

main();
