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
    chalk.blue('=== ignoreFolderPath Update Test ==='),
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
      // userDataPath does not affect updates: managed files update normally
      { path: 'User/settings.json', contains: '"from":"v2"' },
      // ignoreFolderPath skips existing files under it on update
      { path: 'cache/keep.dat', contains: 'CACHE_MODIFIED' },
      { path: 'cache/local-only.dat', contains: 'LOCAL_ONLY' },
    ]);

    const skippedNew = await verifyFilesRemoved(testDir, ['cache/new.dat']);

    const allPassed =
      kept.failed.length === 0 && skippedNew.failed.length === 0;

    if (allPassed) {
      console.log(
        chalk.green('✓ ignoreFolderPath honored on update, userDataPath files updated'),
      );
    } else {
      console.error(chalk.red('✗ ignore verification failed:'));
      [...kept.failed, ...skippedNew.failed].forEach((msg) =>
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
