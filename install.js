const https = require('https');
const http = require('http');
const fs = require('fs');
const path = require('path');
const os = require('os');

const VERSION = '0.0.1';
const REPO = 'suryavirkapur/clawedcode';

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
  return new Promise((resolve, reject) => {
    const protocol = url.startsWith('https') ? https : http;
    const file = fs.createWriteStream(dest);

    protocol.get(url, (response) => {
      if (response.statusCode === 302 || response.statusCode === 301) {
        download(response.headers.location, dest).then(resolve).catch(reject);
        return;
      }
      if (response.statusCode !== 200) {
        reject(new Error(`Failed to download: ${response.statusCode}`));
        return;
      }
      response.pipe(file);
      file.on('finish', () => {
        file.close();
        fs.chmodSync(dest, 0o755);
        resolve();
      });
    }).on('error', reject);
  });
}

async function main() {
  const { osName, target, ext } = getPlatform();
  const binName = `clawedcode-${target}-${osName}${ext}`;
  const url = `https://github.com/${REPO}/releases/download/v${VERSION}/${binName}`;
  const binDir = path.join(__dirname, 'bin');
  const binPath = path.join(binDir, `clawedcode-bin${ext}`);

  if (!fs.existsSync(binDir)) {
    fs.mkdirSync(binDir, { recursive: true });
  }

  console.log(`Downloading clawedcode ${VERSION} for ${target}-${osName}...`);
  await download(url, binPath);
  console.log(`Installed to ${binPath}`);
}

main().catch((err) => {
  console.error('Failed to install clawedcode:', err.message);
  process.exit(1);
});
