import {
  verifyFiles,
  cleanupTestDir,
  getTestDir,
  getFileHash,
  FLAGS,
  runInstaller,
  assertExitOk,
  clearLogFile,
} from './utils.mjs';
import 'zx/globals';
import { usePwsh } from 'zx';
usePwsh();
// zx globals: chalk, path, fs

async function test() {
  const testDir = getTestDir('already-latest');
  const installerPath = './fixtures/test-app-v1.exe';

  console.log(chalk.blue('=== Already Latest Test ==='));
  console.log(`Test directory: ${testDir}`);

  try {
    console.log('Installing v1...');
    await clearLogFile();
    let result = await runInstaller(
      installerPath,
      [FLAGS, '-D', testDir],
      'V1 installation',
    );
    assertExitOk(result, 'V1 installation');

    const appHashBefore = await getFileHash(path.join(testDir, 'app.exe'));

    console.log('Running installer again on the same directory...');
    await clearLogFile();
    result = await runInstaller(
      installerPath,
      [FLAGS, '-D', testDir],
      'Already-latest run',
    );
    assertExitOk(result, 'Already-latest run');

    const appHashAfter = await getFileHash(path.join(testDir, 'app.exe'));
    const extraFailed = [];
    if (appHashBefore !== appHashAfter) {
      extraFailed.push(
        `app.exe changed on already-latest run: ${appHashBefore} -> ${appHashAfter}`,
      );
    }

    const verification = await verifyFiles(testDir, [
      { path: 'app.exe', contains: 'APP_V1' },
      { path: 'config.json', contains: '"version": "1.0.0"' },
      { path: 'readme.txt', contains: 'v1.0.0' },
    ]);

    if (verification.failed.length === 0 && extraFailed.length === 0) {
      console.log(chalk.green('✓ Already-latest run left files unchanged'));
    } else {
      console.error(chalk.red('✗ Already-latest verification failed:'));
      [...verification.failed, ...extraFailed].forEach((msg) =>
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
