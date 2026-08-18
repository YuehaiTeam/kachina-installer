import {
  verifyFiles,
  cleanupTestDir,
  getTestDir,
  waitForServer,
  runInstaller,
  assertExitOk,
  clearLogFile,
  assertNoInstallerError,
  reportVerification,
} from './utils.mjs';
import express from 'express';
import 'zx/globals';
import { usePwsh } from 'zx';
usePwsh();

const PORT = process.env.PORT || 8080;
const FIXTURES_DIR = path.resolve('./fixtures');

function startStubServer() {
  const seen = [];
  const app = express();
  app.use((req, res, next) => {
    seen.push(req.originalUrl);
    console.log(`${req.method} ${req.originalUrl}`);
    if (req.path === '/test-app-v1.exe' && req.query.from !== 'stub') {
      res.status(404).send('stub query required');
      return;
    }
    next();
  });
  app.use(
    express.static(FIXTURES_DIR, {
      acceptRanges: true,
      lastModified: true,
      etag: true,
    }),
  );
  return new Promise((resolve) => {
    const server = app.listen(PORT, () => {
      console.log(chalk.green(`Stub server listening on port ${PORT}`));
      resolve({
        close: () =>
          new Promise((done) => {
            server.close(() => done());
          }),
        seen,
      });
    });
  });
}

async function test() {
  const testDir = getTestDir('plugin-stub');
  const installerPath = './fixtures/test-app-v1.exe';

  console.log(chalk.blue('=== Silent stub plugin WebView Test ==='));
  console.log(`Test directory: ${testDir}`);

  const server = await startStubServer();

  try {
    await waitForServer('http://localhost:8080/test-app-v1.exe?from=stub');
    await clearLogFile();

    console.log('Running silent install via plugin-stub source...');
    const result = await runInstaller(
      installerPath,
      ['-S', '-O', '-D', testDir, '--source', 'stub-v1'],
      'Stub plugin silent install',
    );
    assertExitOk(result, 'Stub plugin silent install');
    await assertNoInstallerError();

    const rewritten = server.seen.filter((url) =>
      url.includes('from=stub'),
    );
    if (rewritten.length === 0) {
      throw new Error(
        `plugin did not rewrite URL; requests: ${server.seen.join(', ') || '(none)'}`,
      );
    }
    const blocked = server.seen.filter(
      (url) =>
        url.includes('/test-app-v1.exe') && !url.includes('from=stub'),
    );
    if (blocked.length === 0) {
      console.log(
        chalk.gray(
          `  Rewritten requests: ${rewritten.slice(0, 3).join(', ')}${
            rewritten.length > 3 ? '…' : ''
          }`,
        ),
      );
    }

    const expectedFiles = [
      { path: 'app.exe', contains: 'APP_V1' },
      { path: 'config.json', contains: '"version": "1.0.0"' },
      { path: 'readme.txt', contains: 'v1.0.0' },
      { path: 'data/assets.dat', size: 10240 },
      { path: 'updater.exe' },
    ];
    console.log('Verifying installed files...');
    const verification = await verifyFiles(testDir, expectedFiles);
    reportVerification('Silent stub plugin install', verification);
  } catch (error) {
    console.error(chalk.red('Test failed:'), error.message);
    process.exit(1);
  } finally {
    await server.close();
    await cleanupTestDir(testDir);
  }
  process.exit(0);
}

test();
