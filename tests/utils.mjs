import crypto from 'crypto';
import { spawn } from 'child_process';
import fs from 'fs-extra';
import path from 'path';
import os from 'os';

export const dev = !!process.env.DEV;
export const FLAGS = dev ? '-I' : '-S';
// NSIS-style switch; keeps the slash-alias path covered end-to-end
export const FLAGS_SLASH = dev ? '-I' : '/S';

export function getTestDir(name) {
  return path.join(os.tmpdir(), `kachina-test-${name}-${Date.now()}`);
}

export async function getFileHash(filePath) {
  const hash = crypto.createHash('sha256');
  const stream = fs.createReadStream(filePath);

  return new Promise((resolve, reject) => {
    stream.on('data', (chunk) => hash.update(chunk));
    stream.on('end', () => resolve(hash.digest('hex')));
    stream.on('error', reject);
  });
}

export async function verifyFiles(installDir, expectedFiles) {
  const results = { passed: [], failed: [] };

  for (const file of expectedFiles) {
    const fullPath = path.join(installDir, file.path);

    // 检查文件是否存在
    if (!(await fs.pathExists(fullPath))) {
      results.failed.push(`Missing file: ${file.path}`);
      continue;
    }

    // 验证文件大小（可选）
    if (file.size) {
      const stats = await fs.stat(fullPath);
      if (stats.size !== file.size) {
        results.failed.push(
          `Size mismatch for ${file.path}: expected ${file.size}, got ${stats.size}`,
        );
        continue;
      }
    }

    // 验证文件内容包含特定字符串（可选）
    if (file.contains) {
      const content = await fs.readFile(fullPath);
      if (!content.includes(file.contains)) {
        results.failed.push(
          `Content mismatch for ${file.path}: missing "${file.contains}"`,
        );
        continue;
      }
    }

    // 验证文件hash（可选）
    if (file.hash) {
      const actualHash = await getFileHash(fullPath);
      if (actualHash !== file.hash) {
        results.failed.push(
          `Hash mismatch for ${file.path}: expected ${file.hash}, got ${actualHash}`,
        );
        continue;
      }
    }

    results.passed.push(file.path);
  }

  return results;
}

export async function verifyFilesRemoved(installDir, removedFiles) {
  const results = { passed: [], failed: [] };

  for (const file of removedFiles) {
    const fullPath = path.join(installDir, file);
    const exists = await fs.pathExists(fullPath);

    if (exists) {
      results.failed.push(`File should be removed but exists: ${file}`);
    } else {
      results.passed.push(file);
    }
  }

  return results;
}

export async function verifyUpdaterReplaced(installDir, expectedV2Hash) {
  const updaterPath = path.join(installDir, 'updater.exe');

  if (!(await fs.pathExists(updaterPath))) {
    return { success: false, message: 'Updater file not found' };
  }

  // 通过hash比对验证更新器是否为v2版本
  const actualHash = await getFileHash(updaterPath);

  if (actualHash === expectedV2Hash) {
    return {
      success: true,
      message: 'Updater successfully self-updated to v2',
    };
  }

  return {
    success: false,
    message: `Updater was not updated to v2. Expected hash: ${expectedV2Hash}, got: ${actualHash}`,
  };
}

export async function cleanupTestDir(testDir) {
  try {
    await fs.remove(testDir);
    console.log(chalk.gray(`Cleaned up: ${testDir}`));
  } catch (error) {
    console.warn(
      chalk.yellow(`Failed to cleanup ${testDir}: ${error.message}`),
    );
  }
}

export async function waitForServer(url, maxAttempts = 10, interval = 1000) {
  for (let i = 0; i < maxAttempts; i++) {
    try {
      const response = await fetch(url, { method: 'HEAD' });
      if (response.ok) {
        return true;
      }
    } catch (error) {
      // 继续尝试
    }
    await new Promise((resolve) => setTimeout(resolve, interval));
  }
  throw new Error(
    `Server at ${url} did not respond after ${maxAttempts} attempts`,
  );
}

export async function printLogFileIfExists() {
  const logFile = path.join(os.tmpdir(), 'KachinaInstaller.log');
  if (await fs.pathExists(logFile)) {
    console.log(chalk.yellow('\n=== Installer Log File Contents ==='));
    const logs = await fs.readFile(logFile, 'utf-8');
    console.log(logs);
    console.log(chalk.yellow('=== End of Log File ===\n'));
  } else {
    console.log(chalk.yellow('Log file not found at: ' + logFile));
  }
}

export function getLogFilePath() {
  return path.join(os.tmpdir(), 'KachinaInstaller.log');
}

export async function clearLogFile() {
  const logFile = getLogFilePath();
  if (await fs.pathExists(logFile)) {
    await fs.remove(logFile);
  }
}

export async function runInstaller(exe, args, label, timeout = '3m') {
  const { $, usePwsh } = await import('zx');
  usePwsh();
  try {
    const result = await $`& ${exe} ${args}`.timeout(timeout);
    return result;
  } catch (error) {
    if (error.message && error.message.includes('timed out')) {
      console.error(chalk.red(`${label} timed out after ${timeout}`));
      await printLogFileIfExists();
    }
    throw error;
  }
}

export function assertDfs2BatchCoverage(state, label) {
  if (state.batchRequests.length === 0) {
    throw new Error(`${label}: DFS2 batch endpoint was not called`);
  }
  if (state.singleRequests.length !== 0) {
    throw new Error(
      `${label}: fell back to ${state.singleRequests.length} single DFS2 URL requests`,
    );
  }
  const batchRanges = new Set(
    state.batchRequests.flatMap((request) => request.ranges),
  );
  const missing = state.downloadRequests
    .map((request) => request.range)
    .filter((range) => !batchRanges.has(range));
  if (missing.length > 0) {
    throw new Error(
      `${label}: downloads were not covered by batch URLs: ${missing.join(', ')}`,
    );
  }
}

export function spawnInstaller(exe, args) {
  return spawn(exe, args, {
    windowsHide: true,
    stdio: 'ignore',
  });
}

export function waitForProcess(child, timeout = 180000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      child.kill();
      reject(
        new Error(`Process ${child.pid ?? '<unknown>'} did not exit in time`),
      );
    }, timeout);
    child.once('error', (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.once('exit', (code, signal) => {
      clearTimeout(timer);
      resolve({ exitCode: code, signal });
    });
  });
}

export function assertExitOk(result, label) {
  if (result.exitCode !== 0) {
    throw new Error(`${label} failed with exit code ${result.exitCode}`);
  }
}

export async function assertNoInstallerError() {
  const logFile = getLogFilePath();
  if (!(await fs.pathExists(logFile))) {
    return;
  }
  const logs = await fs.readFile(logFile, 'utf-8');
  console.log(logs);
  if (logs.includes('ERROR kachina_installer::installer')) {
    throw new Error('Installer log contains errors');
  }
}

export function reportVerification(name, verification, extraFailed = []) {
  const failed = [...verification.failed, ...extraFailed];
  if (failed.length === 0) {
    console.log(chalk.green(`✓ ${name}`));
    if (verification.passed?.length) {
      console.log(chalk.gray(`  Verified: ${verification.passed.join(', ')}`));
    }
    return;
  }
  console.error(chalk.red(`✗ ${name} failed:`));
  failed.forEach((msg) => console.error(chalk.red(`  - ${msg}`)));
  process.exit(1);
}
