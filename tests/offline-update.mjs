import {
  verifyFiles,
  verifyFilesRemoved,
  cleanupTestDir,
  getTestDir,
  printLogFileIfExists,
  FLAGS,
} from './utils.mjs';
import 'zx/globals';
import { $, usePwsh } from 'zx';
usePwsh();

async function test() {
  const testDir = getTestDir('offline-update');
  const installerV1 = './fixtures/test-app-v1.exe';
  const installerV2 = './fixtures/test-app-v2.exe';

  console.log(chalk.blue('=== Offline Update Test ==='));
  console.log(`Test directory: ${testDir}`);

  try {
    // 步骤1: 安装v1
    console.log('Installing v1...');
    let result;
    try {
      const prom = $`${installerV1} ${FLAGS} -D ${testDir}`.timeout('3m').quiet();
      result = await Promise.race([
        prom,
        new Promise((_, reject) =>
          setTimeout(
            () => reject(new Error('V1 installation timed out after 3 minutes')),
            3 * 60 * 1000,
          ),
        ),
      ]);
    } catch (error) {
      if (error.message && error.message.includes('timed out')) {
        console.error(chalk.red('V1 installation timed out after 3 minutes'));
        await printLogFileIfExists();
      }
      throw error;
    }
    if (result.exitCode !== 0) {
      throw new Error(
        `V1 installation failed with exit code ${result.exitCode}`,
      );
    }

    // 步骤2: 使用v2包进行更新
    console.log('Updating to v2...');
    try {
      const prom = $`${installerV2} ${FLAGS} -D ${testDir}`.timeout('3m').quiet();
      result = await Promise.race([
        prom,
        new Promise((_, reject) =>
          setTimeout(
            () => reject(new Error('Update to v2 timed out after 3 minutes')),
            3 * 60 * 1000,
          ),
        ),
      ]);
    } catch (error) {
      if (error.message && error.message.includes('timed out')) {
        console.error(chalk.red('Update to v2 timed out after 3 minutes'));
        await printLogFileIfExists();
      }
      throw error;
    }
    if (result.exitCode !== 0) {
      throw new Error(`Update to v2 failed with exit code ${result.exitCode}`);
    }

    // 验证v2文件存在
    const expectedFiles = [
      { path: 'app.exe', contains: 'APP_V2' },
      { path: 'config.json', contains: '"version": "2.0.0"' },
      { path: 'feature.dll', size: 30720 },
      { path: 'data/assets.dat', size: 15360 },
      { path: 'data/new-assets.dat', size: 5120 },
      { path: 'updater.exe' },
    ];

    console.log('Verifying v2 files...');
    const verification = await verifyFiles(testDir, expectedFiles);

    // 验证v1独有文件被删除
    const removedFiles = ['readme.txt'];
    const removalVerification = await verifyFilesRemoved(testDir, removedFiles);

    // 输出结果
    const allPassed =
      verification.failed.length === 0 &&
      removalVerification.failed.length === 0;

    if (allPassed) {
      console.log(chalk.green('✓ Update completed successfully'));
      console.log(
        chalk.gray(`  New/Updated files: ${verification.passed.join(', ')}`),
      );
      console.log(
        chalk.gray(`  Removed files: ${removalVerification.passed.join(', ')}`),
      );
    } else {
      console.error(chalk.red('✗ Update verification failed:'));
      verification.failed.forEach((msg) =>
        console.error(chalk.red(`  - ${msg}`)),
      );
      removalVerification.failed.forEach((msg) =>
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
