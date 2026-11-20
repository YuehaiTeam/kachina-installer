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

// The v2 fixture's \0META carries a real data_split_hint written by pack: no
// declared or requested range may cross a window boundary, and at least one
// merged download must land strictly inside a hint window.
async function testSplitHint() {
  const label = 'DFS2 split-hint installation';
  const server = await startServer();
  const testDir = getTestDir('dfs2-split-hint');
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
    const resource = server.dfs2State.getResource('v2');
    const hint = resource.data.metadata.data_split_hint;
    if (!Array.isArray(hint) || hint.length === 0) {
      throw new Error(`${label}: fixture metadata has no data_split_hint`);
    }
    const windows = hint.map(([start, end]) => [
      start + resource.dataEnd,
      end + resource.dataEnd,
    ]);
    const declared = Array.from(server.dfs2State.sessions.values()).flatMap(
      (session) => session.declared ?? [],
    );
    const requested = server.dfs2State.batchRequests.flatMap(
      (request) => request.ranges,
    );
    const crossing = [...declared, ...requested].filter((range) => {
      const [start, end] = range.split('-').map(Number);
      return (
        start > end ||
        windows.some(([w0, w1]) => (start < w0 && w0 <= end) || (start < w1 && w1 <= end))
      );
    });
    if (crossing.length) {
      throw new Error(
        `${label}: ranges crossing a data_split_hint boundary: ${crossing.join(', ')}`,
      );
    }
    const inside = declared.some((range) => {
      const [start, end] = range.split('-').map(Number);
      return windows.some(([w0, w1]) => w0 < start && end < w1);
    });
    if (!inside) {
      throw new Error(
        `${label}: no declared range sits strictly inside a hint window`,
      );
    }
  } finally {
    await cleanupTestDir(testDir);
    await new Promise((resolve) => server.close(resolve));
  }
}

// 拆包产物(cas/{xxh} + v2.json)直接放在静态 HTTP 上：客户端把 {ver}.json
// 当成 dfs2 data 载荷读，按 extents 自行把逻辑区间映射成 cas 文件的
// `#range=` 物理请求，不经任何调度器。
async function testExtentInstall() {
  const label = 'DFS2 extent-source installation';
  const testDir = getTestDir('dfs2-extent');
  cleanupTestDir(testDir);
  await fs.ensureDir(testDir);
  const server = await startServer();
  try {
    const result = await runInstaller(
      './fixtures/test-app-v1.exe',
      [FLAGS, '-O', '-D', testDir, '--source', 'split-v2'],
      label,
    );
    assertExitOk(result, label);
    await verifyV2(testDir);

    const casRequests = server.dfs2State.requests.filter((r) =>
      r.includes('GET /split/cas/'),
    );
    if (!casRequests.length) {
      throw new Error(`${label}: no cas/ file was requested`);
    }
    const stray = server.dfs2State.requests.filter(
      (r) => r.includes('/split/') && !r.includes('/split/cas/') && !r.includes('/split/v2.json'),
    );
    if (stray.length) {
      throw new Error(`${label}: unexpected requests ${stray.join(', ')}`);
    }
    if (server.dfs2State.requests.some((r) => r.includes('/api/'))) {
      throw new Error(`${label}: scheduler was touched without dfs2`);
    }
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
}

async function main() {
  if (scenario === 'online-install-extent') {
    await testExtentInstall();
    console.log(`✓ ${scenario}`);
    return;
  }
  if (scenario === 'online-install-dfs2-sliced') {
    await testSlicedInstall();
    console.log(`✓ ${scenario}`);
    return;
  }
  if (scenario === 'online-install-dfs2-split-hint') {
    await testSplitHint();
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
