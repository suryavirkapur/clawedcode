const fs = require('fs');
const path = require('path');
const os = require('os');
const { Readable } = require('stream');
const { pipeline } = require('stream/promises');

const VERSION = require('./package.json').version;
const REPO = 'suryavirkapur/clawedcode';
const DOWNLOAD_TIMEOUT_MS = 120000;

function getPlatform() {
  const platform = os.platform();
  const arch = os.arch();

  let osName;
  switch (platform) {
    case 'linux': osName = 'linux'; break;
    case 'darwin': osName = 'darwin'; break;
    case 'win32': osName = 'windows'; break;
    default:
      console.error(`Unsupported OS: ${platform}`);
      process.exit(1);
  }

  let target;
  switch (arch) {
    case 'x64': target = 'x86_64'; break;
    case 'arm64': target = 'aarch64'; break;
    default:
      console.error(`Unsupported architecture: ${arch}`);
      process.exit(1);
  }

  const ext = osName === 'windows' ? '.exe' : '';
  return { osName, target, ext };
}

function download(url, dest) {
  const tmpDest = `${dest}.tmp`;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), DOWNLOAD_TIMEOUT_MS);

  return (async () => {
    try {
      const response = await fetch(url, {
        redirect: 'follow',
        signal: controller.signal,
        headers: {
          'user-agent': `clawedcode-installer/${VERSION}`,
        },
      });

      if (!response.ok) {
        throw new Error(`Failed to download: ${response.status} ${response.statusText}`);
      }
      if (!response.body) {
        throw new Error('Download response did not include a body');
      }

      await pipeline(
        Readable.fromWeb(response.body),
        fs.createWriteStream(tmpDest),
      );

      fs.renameSync(tmpDest, dest);
      fs.chmodSync(dest, 0o755);
    } catch (err) {
      fs.rmSync(tmpDest, { force: true });
      if (err?.name === 'AbortError') {
        throw new Error(`Download timed out after ${DOWNLOAD_TIMEOUT_MS / 1000}s`);
      }
      throw err;
    } finally {
      clearTimeout(timeout);
    }
  })();
}

async function main() {
  if (process.env.CLAWEDCODE_SKIP_DOWNLOAD === '1') {
    console.log('Skipping binary download (CLAWEDCODE_SKIP_DOWNLOAD=1)');
    return;
  }

  const { osName, target, ext } = getPlatform();
  const binName = `clawedcode-${target}-${osName}${ext}`;
  const binDir = path.join(__dirname, 'bin');
  const binPath = path.join(binDir, `clawedcode-bin${ext}`);

  if (!fs.existsSync(binDir)) {
    fs.mkdirSync(binDir, { recursive: true });
  }

  const assetDir = process.env.CLAWEDCODE_ASSET_DIR;
  if (assetDir) {
    const src = path.join(assetDir, binName);
    if (!fs.existsSync(src)) {
      throw new Error(`Asset not found: ${src}`);
    }
    fs.copyFileSync(src, binPath);
    fs.chmodSync(binPath, 0o755);
    console.log(`Installed from local assets: ${src}`);
    return;
  }

  const url = `https://github.com/${REPO}/releases/download/v${VERSION}/${binName}`;
  console.log(`Downloading clawedcode ${VERSION} for ${target}-${osName}...`);
  await download(url, binPath);
  console.log(`Installed to ${binPath}`);
}

main().catch((err) => {
  console.error('Failed to install clawedcode:', err.message);
  process.exit(1);
});
