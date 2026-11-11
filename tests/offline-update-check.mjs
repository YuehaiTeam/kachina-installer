// An offline package run unattended installs what it carries without asking
// its source: the server at the source address receives no request.
import http from 'http';
import {
  assertExitOk,
  cleanupTestDir,
  FLAGS,
  getTestDir,
  runInstaller,
  verifyFiles,
} from './utils.mjs';
import 'zx/globals';
import { usePwsh } from 'zx';

usePwsh();

const PACKAGE = './fixtures/test-app-v1.exe';

// Accepts connections and never answers: an install that did ask would be
// recorded here, and would wait for its update check to give up.
function startSilentServer() {
  const requests = [];
  const server = http.createServer((req) => {
    requests.push(`${req.method} ${req.url} ${req.headers.range ?? ''}`.trim());
  });
  return new Promise((resolve) => {
    server.listen(8080, () => resolve({ server, requests }));
  });
}

async function main() {
  const { server, requests } = await startSilentServer();
  const testDir = getTestDir('offline-update-check');
  try {
    const result = await runInstaller(
      PACKAGE,
      [FLAGS, '-D', testDir],
      'offline install beside a silent source',
    );
    assertExitOk(result, 'offline install beside a silent source');
    const verification = await verifyFiles(testDir, [
      { path: 'app.exe', contains: 'APP_V1' },
      { path: 'config.json', contains: '"version": "1.0.0"' },
      { path: 'updater.exe' },
    ]);
    if (verification.failed.length) {
      throw new Error(verification.failed.join('; '));
    }
    if (requests.length !== 0) {
      throw new Error(`unexpected requests: ${requests.join(' | ')}`);
    }
    console.log(chalk.green('✓ the source received no request'));
  } finally {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
    await cleanupTestDir(testDir);
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
