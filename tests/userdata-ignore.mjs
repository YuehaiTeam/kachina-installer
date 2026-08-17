import {
  verifyFiles,
  verifyFilesRemoved,
  cleanupTestDir,
  getTestDir,
  FLAGS,
  runInstaller,
  assertExitOk,
  clearLogFile,
} from './utils.mjs';
import 'zx/globals';
import { usePwsh } from 'zx';
usePwsh();

async function test() {
  const testDir = getTestDir('userdata-ignore');
  const installerV1 = './fixtures/test-app-v1.exe';
  const installerV2 = './fixtures/test-app-v2.exe';

  console.log(
    chalk.blue('=== userDataPath / ignoreFolderPath Update Test ==='),
  );
  console.log(`Test directory: ${testDir}`);

  try {
    console.log('Installing v1...');
    await clearLogFile();
    let result = await runInstaller(
      installerV1,
      [FLAGS, '-D', testDir],
      'V1 installation',
    );
    assertExitOk(result, 'V1 installation');

    await fs.writeFile(
      path.join(testDir, 'User/settings.json'),
      '{"from":"USER_MODIFIED"}\n',
    );
    await fs.writeFile(path.join(testDir, 'cache/keep.dat'), 'CACHE_MODIFIED');
    await fs.writeFile(
      path.join(testDir, 'cache/local-only.dat'),
      'LOCAL_ONLY',
    );

    console.log('Updating to v2...');
    await clearLogFile();
    result = await runInstaller(
      installerV2,
      [FLAGS, '-D', testDir],
      'Update to v2',
    );
    assertExitOk(result, 'Update to v2');

    const kept = await verifyFiles(testDir, [
      { path: 'app.exe', contains: 'APP_V2' },
      { path: 'config.json', contains: '"version": "2.0.0"' },
      { path: 'User/settings.json', contains: 'USER_MODIFIED' },
      { path: 'cache/keep.dat', contains: 'CACHE_MODIFIED' },
      { path: 'cache/local-only.dat', contains: 'LOCAL_ONLY' },
    ]);

    const skippedNew = await verifyFilesRemoved(testDir, ['cache/new.dat']);

    const extraFailed = [];
    const settings = await fs.readFile(
      path.join(testDir, 'User/settings.json'),
      'utf-8',
    );
    if (settings.includes('"from":"v2"')) {
      extraFailed.push(
        'User/settings.json was overwritten by v2 (userDataPath should skip existing user files)',
      );
    }
    const cacheKeep = await fs.readFile(
      path.join(testDir, 'cache/keep.dat'),
      'utf-8',
    );
    if (cacheKeep.includes('CACHE_V2')) {
      extraFailed.push(
        'cache/keep.dat was overwritten by v2 (ignoreFolderPath should skip non-empty cache)',
      );
    }

    const allPassed =
      kept.failed.length === 0 &&
      skippedNew.failed.length === 0 &&
      extraFailed.length === 0;

    if (allPassed) {
      console.log(
        chalk.green('✓ userDataPath and ignoreFolderPath honored on update'),
      );
    } else {
      console.error(chalk.red('✗ userData/ignore verification failed:'));
      [...kept.failed, ...skippedNew.failed, ...extraFailed].forEach((msg) =>
        console.error(chalk.red(`  - ${msg}`)),
      );
      process.exit(1);
    }
  } catch (error) {
    console.error(chalk.red('Test failed:'), error.message);
    process.exit(1);
  } finally {
    await cleanupTestDir(testDir);
  }
  process.exit(0);
}

test();
