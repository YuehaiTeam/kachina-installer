import {
  verifyFiles,
  cleanupTestDir,
  getTestDir,
  FLAGS,
  runInstaller,
  assertExitOk,
  clearLogFile,
} from './utils.mjs';
import { spawn } from 'child_process';
import 'zx/globals';
import { usePwsh } from 'zx';
usePwsh();

function startOccupiedApp(appPath) {
  const child = spawn(appPath, ['/c', 'ping', '127.0.0.1', '-n', '120'], {
    windowsHide: true,
    stdio: 'ignore',
  });
  if (child.pid == null) {
    throw new Error('Failed to start occupied app.exe');
  }
  return child;
}

async function isPidRunning(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function test() {
  const testDir = getTestDir('occupied-process');
  const installerV1 = './fixtures/test-app-v1.exe';
  const installerV2 = './fixtures/test-app-v2.exe';
  let child = null;

  console.log(chalk.blue('=== Occupied Process Test ==='));
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

    const appPath = path.join(testDir, 'app.exe');
    await fs.copyFile('C:/Windows/System32/cmd.exe', appPath);
    child = startOccupiedApp(appPath);
    await new Promise((resolve) => setTimeout(resolve, 1000));
    if (!(await isPidRunning(child.pid))) {
      throw new Error('Occupied app.exe exited before update');
    }
    console.log(chalk.gray(`  Occupied app.exe pid=${child.pid}`));

    console.log('Updating to v2 while app.exe is running...');
    await clearLogFile();
    result = await runInstaller(
      installerV2,
      [FLAGS, '-D', testDir],
      'Update with occupied process',
    );
    assertExitOk(result, 'Update with occupied process');

    await new Promise((resolve) => setTimeout(resolve, 500));
    const stillRunning = await isPidRunning(child.pid);
    const extraFailed = [];
    if (stillRunning) {
      extraFailed.push(`app.exe pid ${child.pid} was not killed`);
    }

    const verification = await verifyFiles(testDir, [
      { path: 'app.exe', contains: 'APP_V2' },
      { path: 'config.json', contains: '"version": "2.0.0"' },
    ]);

    if (verification.failed.length === 0 && extraFailed.length === 0) {
      console.log(chalk.green('✓ Occupied process was ended and update completed'));
    } else {
      console.error(chalk.red('✗ Occupied process verification failed:'));
      [...verification.failed, ...extraFailed].forEach((msg) =>
        console.error(chalk.red(`  - ${msg}`)),
      );
      process.exit(1);
    }
  } catch (error) {
    console.error(chalk.red('Test failed:'), error.message);
    process.exit(1);
  } finally {
    if (child?.pid && (await isPidRunning(child.pid))) {
      try {
        process.kill(child.pid);
      } catch {
        // ignore
      }
    }
    await cleanupTestDir(testDir);
  }
  process.exit(0);
}

test();
