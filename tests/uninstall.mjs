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
  const testDir = getTestDir('uninstall');
  const installerPath = './fixtures/test-app-v1.exe';

  console.log(chalk.blue('=== Uninstall Test ==='));
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

    await fs.ensureDir(path.join(testDir, 'User'));
    await fs.writeFile(
      path.join(testDir, 'User/runtime-created.json'),
      '{"created":"after-install"}\n',
    );
    await fs.ensureDir(path.join(testDir, 'log'));
    await fs.writeFile(path.join(testDir, 'log/debug.log'), 'LOG_LINE\n');

    const uninstallerPath = path.join(testDir, 'uninstall.exe');
    if (!(await fs.pathExists(uninstallerPath))) {
      throw new Error('uninstall.exe was not created');
    }

    console.log('Running silent uninstall...');
    await clearLogFile();
    result = await runInstaller(uninstallerPath, [FLAGS], 'Uninstall');
    assertExitOk(result, 'Uninstall');

    const removed = await verifyFilesRemoved(testDir, [
      'app.exe',
      'config.json',
      'readme.txt',
      'data/assets.dat',
      'updater.exe',
      'log/debug.log',
    ]);

    const kept = await verifyFiles(testDir, [
      { path: 'User/runtime-created.json', contains: 'after-install' },
    ]);

    const allPassed = removed.failed.length === 0 && kept.failed.length === 0;
    if (allPassed) {
      console.log(chalk.green('✓ Uninstall removed package files and extraUninstallPath'));
      console.log(chalk.green('✓ User runtime data kept (deleteUserData defaults to false)'));
    } else {
      console.error(chalk.red('✗ Uninstall verification failed:'));
      [...removed.failed, ...kept.failed].forEach((msg) =>
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
