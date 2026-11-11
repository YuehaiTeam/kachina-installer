import {
  verifyFiles,
  cleanupTestDir,
  getTestDir,
  FLAGS_SLASH,
  runInstaller,
  assertExitOk,
  assertStagingRemoved,
  removeUserShortcuts,
  verifyUserShortcuts,
} from './utils.mjs';
import 'zx/globals';
import { usePwsh } from 'zx';
usePwsh();

async function test() {
  const testDir = getTestDir('offline-install');
  const installerPath = './fixtures/test-app-v1.exe';

  console.log(chalk.blue('=== Offline Installation Test ==='));
  console.log(`Test directory: ${testDir}`);
  console.log(`Installer: ${installerPath}`);

  // 先清掉别的测试留下的同名快捷方式，安装后读到的只能是本次建的
  const APP_NAME = 'Test Application';
  try {
    await removeUserShortcuts(APP_NAME);
    // 执行离线安装
    console.log('Running offline installation...');
    const result = await runInstaller(
      installerPath,
      [FLAGS_SLASH, '-D', testDir],
      'Offline installation',
    );
    assertExitOk(result, 'Offline installation');

    // 验证安装的文件
    const expectedFiles = [
      { path: 'app.exe', contains: 'APP_V1' },
      { path: 'config.json', contains: '"version": "1.0.0"' },
      { path: 'readme.txt', contains: 'v1.0.0' },
      { path: 'data/assets.dat', size: 10240 },
      { path: 'updater.exe' }, // v1更新器
    ];

    console.log('Verifying installed files...');
    const verification = await verifyFiles(testDir, expectedFiles);
    verification.failed.push(...(await verifyUserShortcuts(APP_NAME, testDir, 'app.exe')));

    // 输出结果
    if (verification.failed.length === 0) {
      await assertStagingRemoved(testDir);
      console.log(chalk.green('✓ All files installed correctly'));
      console.log(chalk.gray(`  Verified: ${verification.passed.join(', ')}`));
    } else {
      console.error(chalk.red('✗ Verification failed:'));
      verification.failed.forEach((msg) =>
        console.error(chalk.red(`  - ${msg}`)),
      );
      process.exit(1);
    }
  } catch (error) {
    console.error(chalk.red('Test failed:'), error.message);
    process.exit(1);
  } finally {
    await removeUserShortcuts(APP_NAME);
    await cleanupTestDir(testDir);
  }
  process.exit(0);
}

test();
