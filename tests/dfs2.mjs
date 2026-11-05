import path from 'path';
import {
  assertDfs2BatchCoverage,
  assertExitOk,
  cleanupTestDir,
  getFileHash,
  getTestDir,
  runInstaller,
  verifyFiles,
  verifyFilesRemoved,
} from './utils.mjs';
import { startServer } from './server.mjs';
import { FLAGS } from './utils.mjs';
import 'zx/globals';
import { usePwsh } from 'zx';

usePwsh();

const scenario = process.argv[2];

async function installFromDfs2(testDir, source) {
  return runInstaller(
    './fixtures/test-app-v1.exe',
    [FLAGS, '-O', '-D', testDir, '--source', source],
    `DFS2 installation from ${source}`,
  );
}

async function installV1(testDir) {
  return runInstaller(
    './fixtures/test-app-v1.exe',
    [FLAGS, '-D', testDir],
    'DFS2 v1 installation',
  );
}

async function updateToV2(testDir) {
  return runInstaller(
    path.join(testDir, 'updater.exe'),
    [FLAGS, '-D', testDir, '--source', 'dfs2-v2'],
    'DFS2 v2 update',
  );
}

async function verifyV2(testDir) {
  const expectedFiles = [
    { path: 'app.exe', contains: 'APP_V2' },
    { path: 'config.json', contains: '"version": "2.0.0"' },
    { path: 'feature.dll', size: 30720 },
    { path: 'data/assets.dat', size: 15360 },
    { path: 'data/new-assets.dat', size: 5120 },
    { path: 'updater.exe' },
  ];
  const verification = await verifyFiles(testDir, expectedFiles);
  const removed = await verifyFilesRemoved(testDir, ['readme.txt']);
  if (verification.failed.length || removed.failed.length) {
    throw new Error([...verification.failed, ...removed.failed].join('; '));
  }
}

async function testInstall(server) {
  const testDir = getTestDir('online-install-dfs2');
  try {
    const result = await installFromDfs2(testDir, 'dfs2-v2');
    assertExitOk(result, 'DFS2 v2 installation');
    const verification = await verifyFiles(testDir, [
      { path: 'app.exe', contains: 'APP_V2' },
      { path: 'config.json', contains: '"version": "2.0.0"' },
      { path: 'feature.dll', size: 30720 },
      { path: 'data/assets.dat', size: 15360 },
      { path: 'data/new-assets.dat', size: 5120 },
      { path: 'updater.exe' },
    ]);
    if (verification.failed.length) {
      throw new Error(verification.failed.join('; '));
    }
    assertDfs2BatchCoverage(server.dfs2State, 'DFS2 installation');
  } finally {
    await cleanupTestDir(testDir);
  }
}

async function testUpdate(server) {
  const testDir = getTestDir('online-update-dfs2');
  try {
    let result = await installV1(testDir);
    assertExitOk(result, 'DFS2 v1 installation');
    const v2UpdaterHash = await getFileHash(
      './fixtures/test-app-v2/updater.exe',
    );
    result = await updateToV2(testDir);
    assertExitOk(result, 'DFS2 v2 update');
    await verifyV2(testDir);
    if (
      (await getFileHash(path.join(testDir, 'updater.exe'))) !== v2UpdaterHash
    ) {
      throw new Error('DFS2 update did not replace updater.exe');
    }
    assertDfs2BatchCoverage(server.dfs2State, 'DFS2 update');
    if (
      !Array.from(server.dfs2State.sessions.values()).some(
        (session) => session.challenged,
      )
    ) {
      throw new Error('DFS2 update did not complete the challenge flow');
    }
  } finally {
    await cleanupTestDir(testDir);
  }
}

const MIB = 1024 * 1024;

// app.exe (about 3 MiB) is the only file over 1 MiB. The online-only updater
// has no embedded files, so a fresh install fetches app.exe whole, and 1 MiB
// part hints cut it into slices.
function appRange(resource) {
  const [, entry] = Object.entries(resource.data.index).find(
    ([key, entry]) => entry.name === 'app.exe' && !key.includes('_'),
  );
  return { start: entry.offset, end: entry.offset + entry.size - 1 };
}

function appDownloads(server) {
  const app = appRange(server.dfs2State.getResource('v2'));
  return server.dfs2State.downloadRequests
    .map((request) => {
      const [start, end] = request.range.split('-').map(Number);
      return { start, end, aborted: request.aborted };
    })
    .filter((request) => request.start >= app.start && request.end <= app.end)
    .map((request) => ({
      ...request,
      whole: request.start === app.start && request.end === app.end,
    }));
}

// The first slice feeds the decoder directly: cut off after it has delivered
// bytes, it fails the attempt, and the next attempt fetches the whole file.
async function testSlicedInstall() {
  const label = 'DFS2 sliced installation';
  const server = await startServer({
    partBytes: MIB,
    abortSigned: (range, resource) => {
      const app = appRange(resource);
      return range.start === app.start && range.end < app.end;
    },
  });
  const testDir = getTestDir('dfs2-sliced');
  try {
    const result = await runInstaller(
      './fixtures/test-app-v1/updater.exe',
      [FLAGS, '-O', '-D', testDir, '--source', 'dfs2-v2'],
      label,
    );
    assertExitOk(result, label);
    const verification = await verifyFiles(testDir, [
      { path: 'app.exe', contains: 'APP_V2' },
      { path: 'feature.dll', size: 30720 },
      { path: 'updater.exe' },
    ]);
    if (verification.failed.length) {
      throw new Error(verification.failed.join('; '));
    }
    assertDfs2BatchCoverage(server.dfs2State, label);
    const downloads = appDownloads(server);
    const first = downloads.find((d) => d.aborted);
    if (
      !first ||
      first.end - first.start + 1 > MIB ||
      !downloads.some((d) => d.whole && !d.aborted)
    ) {
      throw new Error(
        `${label}: app.exe was not fetched in slices of at most 1 MiB and then whole: ${JSON.stringify(downloads)}`,
      );
    }
  } finally {
    await cleanupTestDir(testDir);
    await new Promise((resolve) => server.close(resolve));
  }
}

async function main() {
  if (scenario === 'online-install-dfs2-sliced') {
    await testSlicedInstall();
    console.log(`✓ ${scenario}`);
    return;
  }
  if (!['online-install-dfs2', 'online-update-dfs2'].includes(scenario)) {
    throw new Error(`Unknown DFS2 scenario: ${scenario}`);
  }
  const server = await startServer();
  try {
    if (scenario === 'online-install-dfs2') {
      await testInstall(server);
    } else if (scenario === 'online-update-dfs2') {
      await testUpdate(server);
    }
    console.log(`✓ ${scenario}`);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
