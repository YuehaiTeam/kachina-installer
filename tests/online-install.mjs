import {
  verifyFiles,
  cleanupTestDir,
  getTestDir,
  waitForServer,
  printLogFileIfExists,
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

    // 删除日志文件 %temp%/KachinaInstaller.log
    const logFile = os.tmpdir() + '/KachinaInstaller.log';
    if (await fs.pathExists(logFile)) {
      await fs.remove(logFile);
    }

    // 执行在线安装
    console.log('Running online installation...');
    let result;
    try {
      result =
        await $`${installerPath} ${FLAGS} -O -D ${testDir} --source local-v1`.timeout('3m').quiet();
    } catch (error) {
      if (error.message && error.message.includes('timed out')) {
        console.error(chalk.red('Installation process timed out after 3 minutes'));
        await printLogFileIfExists();
      }
      throw error;
    }

    if (result.exitCode !== 0) {
      throw new Error(`Installation failed with exit code ${result.exitCode}`);
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
  process.exit(0);
}

test();
