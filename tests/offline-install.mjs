import { verifyFiles, cleanupTestDir, getTestDir, FLAGS } from './utils.mjs';
import 'zx/globals';
import { $, usePwsh } from 'zx';
usePwsh();

async function test() {
  const testDir = getTestDir('offline-install');
  const installerPath = './fixtures/test-app-v1.exe';

  console.log(chalk.blue('=== Offline Installation Test ==='));
  console.log(`Test directory: ${testDir}`);
  console.log(`Installer: ${installerPath}`);

  try {
    // 执行离线安装
    console.log('Running offline installation...');
    const result = await $`${installerPath} ${FLAGS} -D ${testDir}`.quiet();

    if (result.exitCode !== 0) {
      throw new Error(`Installation failed with exit code ${result.exitCode}`);
    }

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

    // 输出结果
    if (verification.failed.length === 0) {
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
    await cleanupTestDir(testDir);
  }
}

test();
