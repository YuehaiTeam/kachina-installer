import crypto from 'crypto';
import fs from 'fs-extra';
import os from 'os';
import path from 'path';
import { startServer } from './server.mjs';
import {
  assertExitOk,
  cleanupTestDir,
  getFileHash,
  getTestDir,
  runInstaller,
  spawnInstaller,
  verifyFiles,
  waitForProcess,
} from './utils.mjs';
import { FLAGS } from './utils.mjs';
import 'zx/globals';
import { usePwsh } from 'zx';

usePwsh();

function stagingRootFor(installDir) {
  const normalized = installDir
    .replaceAll('/', '\\')
    .replace(/[\\]+$/, '')
    .toLowerCase();
  const hash = crypto
    .createHash('sha256')
    .update(normalized)
    .digest('hex')
    .slice(0, 16);
  return path.join(os.tmpdir(), 'kachina-staged', hash);
}

async function installV1(testDir) {
  return runInstaller(
    './fixtures/test-app-v1.exe',
    [FLAGS, '-D', testDir],
    'v1 installation',
  );
}

async function verifyV1(testDir, updaterHash) {
  const verification = await verifyFiles(testDir, [
    { path: 'app.exe', contains: 'APP_V1' },
    { path: 'config.json', contains: '"version": "1.0.0"' },
  ]);
  if (verification.failed.length > 0) {
    throw new Error(verification.failed.join('; '));
  }
  const actualUpdaterHash = await getFileHash(
    path.join(testDir, 'updater.exe'),
  );
  if (actualUpdaterHash !== updaterHash) {
    throw new Error(
      'The installed updater is no longer the known-good v1 image',
    );
  }
}

async function verifyV2(testDir) {
  const verification = await verifyFiles(testDir, [
    { path: 'app.exe', contains: 'APP_V2' },
    { path: 'config.json', contains: '"version": "2.0.0"' },
    { path: 'feature.dll', size: 30720 },
    { path: 'data/assets.dat', size: 15360 },
    { path: 'data/new-assets.dat', size: 5120 },
    { path: 'updater.exe' },
  ]);
  if (verification.failed.length > 0) {
    throw new Error(verification.failed.join('; '));
  }
}

async function main() {
  const testDir = getTestDir('updater-survival');
  process.env.KACHINA_E2E_ABORT_VERSION = 'v2';
  process.env.KACHINA_E2E_ABORT_AFTER = '2';
  const server = await startServer();
  try {
    let result = await installV1(testDir);
    assertExitOk(result, 'v1 installation');
    const v1UpdaterHash = await getFileHash(path.join(testDir, 'updater.exe'));

    const failedUpdate = spawnInstaller(path.join(testDir, 'updater.exe'), [
      FLAGS,
      '-D',
      testDir,
      '--source',
      'local-v2',
    ]);
    result = await waitForProcess(failedUpdate);
    if (result.exitCode === 0) {
      throw new Error('The interrupted network update unexpectedly succeeded');
    }
    if (!server.dfs2State.faults.httpAbortInjected) {
      throw new Error('The test server did not inject the network failure');
    }
    const v2Requests = server.dfs2State.httpRequests.filter(
      (request) => request.version === 'v2',
    );
    if (v2Requests.length < 3 || v2Requests[2].requestNumber !== 3) {
      throw new Error(
        'The network failure did not reach a v2 download request',
      );
    }
    await verifyV1(testDir, v1UpdaterHash);
    if (await fs.pathExists(stagingRootFor(testDir))) {
      throw new Error('The failed update left a staging directory');
    }

    delete process.env.KACHINA_E2E_ABORT_VERSION;
    delete process.env.KACHINA_E2E_ABORT_AFTER;
    result = await runInstaller(
      path.join(testDir, 'updater.exe'),
      [FLAGS, '-D', testDir, '--source', 'local-v2'],
      'retry after network failure',
    );
    assertExitOk(result, 'retry after network failure');
    await verifyV2(testDir);
    console.log('✓ updater-survival');
  } finally {
    delete process.env.KACHINA_E2E_ABORT_VERSION;
    delete process.env.KACHINA_E2E_ABORT_AFTER;
    await new Promise((resolve) => server.close(resolve));
    await cleanupTestDir(testDir);
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
