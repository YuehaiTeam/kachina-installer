import {
  verifyFiles,
  verifyFilesRemoved,
  verifyUpdaterReplaced,
  cleanupTestDir,
  getTestDir,
  waitForServer,
  getFileHash,
  printLogFileIfExists,
  FLAGS,
} from './utils.mjs';
import { startServer } from './server.mjs';
import path from 'path';
import { usePwsh, $ } from 'zx';
import 'zx/globals';
usePwsh();
$.verbose = true;

async function test() {
  const testDir = getTestDir('online-update');
  const installerV1 = './fixtures/test-app-v1.exe';

  console.log(
    chalk.blue('=== Online Update Test (with updater self-patch) ==='),
  );
  console.log(`Test directory: ${testDir}`);

  // 获取v2更新器的hash用于验证
  const v2UpdaterHash = await getFileHash('./fixtures/test-app-v2/updater.exe');
  console.log(chalk.gray(`Expected v2 updater hash: ${v2UpdaterHash}`));

  // 启动HTTP服务器
  console.log('Starting HTTP server...');
  const server = await startServer();

  try {
    // 等待服务器启动
    await waitForServer('http://localhost:8080/test-app-v2.exe');

    // 步骤1: 安装v1
    console.log('Installing v1...');
    let result;
    try {
      result = await $`${installerV1} -S -D ${testDir}`.timeout('3m').quiet();
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

    // 步骤2: 从服务器获取v2进行更新
    // 删除日志文件 %temp%/KachinaInstaller.log
    const logFile = os.tmpdir() + '/KachinaInstaller.log';
    if (await fs.pathExists(logFile)) {
      await fs.remove(logFile);
    }
    console.log('Updating to v2 from server...');
    const updaterPath = path.join(testDir, 'updater.exe');
    try {
      result = await $`& ${updaterPath} ${FLAGS} -D ${testDir} --source local-v2`.timeout('3m');
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

    // check if fail in logs
    if (await fs.pathExists(logFile)) {
      const logs = await fs.readFile(logFile, 'utf-8');
      console.log(logs);
      // 验证日志文件是否有错误
      if (logs.includes('ERROR kachina_installer::installer')) {
        throw new Error('Updater log contains errors');
      }
    }

    // 验证v2文件
    const expectedFiles = [
      { path: 'app.exe', contains: 'APP_V2' },
      { path: 'config.json', contains: '"version": "2.0.0"' },
      { path: 'feature.dll', size: 30720 },
      { path: 'data/assets.dat', size: 15360 },
      { path: 'data/new-assets.dat', size: 5120 },
      { path: 'updater.exe' }, // 更新器本身
    ];

    console.log('Verifying v2 files...');
    const verification = await verifyFiles(testDir, expectedFiles);

    // 验证更新器自我更新（通过hash比对）
    console.log('Verifying updater self-patch...');
    const updaterCheck = await verifyUpdaterReplaced(testDir, v2UpdaterHash);

    // 验证文件删除
    const removedFiles = ['readme.txt'];
    const removalVerification = await verifyFilesRemoved(testDir, removedFiles);

    // 输出结果
    const allPassed =
      verification.failed.length === 0 &&
      removalVerification.failed.length === 0 &&
      updaterCheck.success;

    if (allPassed) {
      console.log(chalk.green('✓ Online update completed successfully'));
      console.log(chalk.green(`✓ ${updaterCheck.message}`));
      console.log(
        chalk.gray(`  Updated files: ${verification.passed.join(', ')}`),
      );
      console.log(
        chalk.gray(`  Removed files: ${removalVerification.passed.join(', ')}`),
      );
    } else {
      console.error(chalk.red('✗ Update verification failed:'));
      if (!updaterCheck.success) {
        console.error(chalk.red(`  - ${updaterCheck.message}`));
      }
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
    // 停止服务器
    server?.close();
    await cleanupTestDir(testDir);
  }
}

test();
