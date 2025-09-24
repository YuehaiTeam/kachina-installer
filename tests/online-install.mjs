import {
  verifyFiles,
  cleanupTestDir,
  getTestDir,
  waitForServer,
  FLAGS,
} from './utils.mjs';
import { startServer } from './server.mjs';
import 'zx/globals';
import { $, usePwsh } from 'zx';
usePwsh();

async function test() {
  const testDir = getTestDir('online-install');
  const installerPath = './fixtures/test-app-v1.exe';

  console.log(chalk.blue('=== Online Installation Test ==='));
  console.log(`Test directory: ${testDir}`);

  // 启动HTTP服务器
  console.log('Starting HTTP server...');
  const server = await startServer();

  try {
    // 等待服务器启动
    await waitForServer('http://localhost:8080/test-app-v1.exe');

    // 执行在线安装
    console.log('Running online installation...');
    const result =
      await $`${installerPath} ${FLAGS} -O -D ${testDir} -${FLAGS}ource local-v1`.quiet();

    if (result.exitCode !== 0) {
      throw new Error(`Installation failed with exit code ${result.exitCode}`);
    }

    // 验证安装的文件
    const expectedFiles = [
      { path: 'app.exe', contains: 'APP_V1' },
      { path: 'config.json', contains: '"version": "1.0.0"' },
      { path: 'readme.txt', contains: 'v1.0.0' },
      { path: 'data/assets.dat', size: 10240 },
      { path: 'updater.exe' },
    ];

    console.log('Verifying installed files...');
    const verification = await verifyFiles(testDir, expectedFiles);

    if (verification.failed.length === 0) {
      console.log(chalk.green('✓ All files installed correctly via HTTP'));
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
    // 停止服务器
    server?.close();
    await cleanupTestDir(testDir);
  }
}

test();
