// An offline package checks its source for a newer version and installs what
// it carries whenever the check fails, without asking the server for any file.
import http from 'http';
import fs from 'fs-extra';
import {
  assertExitOk,
  cleanupTestDir,
  clearLogFile,
  FLAGS,
  getLogFilePath,
  getTestDir,
  runInstaller,
  verifyFiles,
} from './utils.mjs';
import 'zx/globals';
import { usePwsh } from 'zx';

usePwsh();

const PACKAGE = './fixtures/test-app-v1.exe';

const FAULTS = {
  'status-500': (_req, res) => {
    res.statusCode = 500;
    res.end('internal error');
  },
  'no-answer': () => {},
};

function startFaultServer(fault) {
  const requests = [];
  const server = http.createServer((req, res) => {
    requests.push(`${req.method} ${req.url} ${req.headers.range ?? ''}`.trim());
    FAULTS[fault](req, res);
  });
  return new Promise((resolve) => {
    server.listen(8080, () => resolve({ server, requests }));
  });
}

async function check(fault) {
  const { server, requests } = await startFaultServer(fault);
  const testDir = getTestDir(`offline-update-check-${fault}`);
  try {
    await clearLogFile();
    const started = Date.now();
    const result = await runInstaller(PACKAGE, [FLAGS, '-D', testDir], fault);
    const elapsed = Date.now() - started;
    assertExitOk(result, fault);
    const verification = await verifyFiles(testDir, [
      { path: 'app.exe', contains: 'APP_V1' },
      { path: 'config.json', contains: '"version": "1.0.0"' },
      { path: 'updater.exe' },
    ]);
    if (verification.failed.length) {
      throw new Error(`${fault}: ${verification.failed.join('; ')}`);
    }
    const log = await fs.readFile(getLogFilePath(), 'utf-8');
    if (!log.includes('online metadata failed')) {
      throw new Error(`${fault}: the update check did not run or did not fail`);
    }
    // the package header only, no file
    if (requests.length !== 1) {
      throw new Error(`${fault}: unexpected requests: ${requests.join(' | ')}`);
    }
    // the check gives up at 5 s; without that the client waits 30 s per read
    if (fault === 'no-answer' && elapsed > 20000) {
      throw new Error(`${fault}: took ${elapsed} ms`);
    }
    console.log(chalk.green(`✓ ${fault}: ${elapsed} ms`));
  } finally {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
    await cleanupTestDir(testDir);
  }
}

async function main() {
  for (const fault of Object.keys(FAULTS)) {
    await check(fault);
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
