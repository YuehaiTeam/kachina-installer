import {
  cleanupTestDir,
  FLAGS,
  runInstaller,
  assertExitOk,
  assertStagingRemoved,
  stagingCandidates,
} from './utils.mjs';
import 'zx/globals';
import { usePwsh } from 'zx';
usePwsh();

// Only the DACL entries: owner/group and the DACL control flags are not what
// decides who can write the installed files.
async function aces(target) {
  const sddl = (await $`(Get-Acl -LiteralPath ${target}).Sddl`).stdout.trim();
  const dacl = sddl.slice(sddl.indexOf('D:') + 2);
  return (dacl.match(/\([^)]*\)/g) ?? []).sort().join('');
}

async function test() {
  // CI runners are elevated, so the installer writes Program Files directly;
  // a staging copy under the user's %TEMP% would carry that directory's ACL.
  // The control lives outside the install directory, which itself may be a
  // renamed staging directory.
  const stamp = Date.now();
  const testDir = path.join(
    process.env.ProgramFiles,
    `kachina-test-acl-${stamp}`,
  );
  const controlDir = `${testDir}-control`;
  const probe = path.join(controlDir, 'probe.txt');

  console.log(chalk.blue('=== Machine ACL Test ==='));
  console.log(`Test directory: ${testDir}`);

  try {
    await fs.ensureDir(controlDir);
    await fs.writeFile(probe, 'probe\n');

    const result = await runInstaller(
      './fixtures/test-app-v1.exe',
      [FLAGS, '-D', testDir],
      'Program Files installation',
    );
    assertExitOk(result, 'Program Files installation');
    await assertStagingRemoved(testDir);

    const expectedDir = await aces(controlDir);
    const expectedFile = await aces(probe);
    const checks = [
      [testDir, expectedDir],
      [path.join(testDir, 'data'), expectedDir],
      [path.join(testDir, 'app.exe'), expectedFile],
      [path.join(testDir, 'data/assets.dat'), expectedFile],
    ];
    const failed = [];
    for (const [target, expected] of checks) {
      const actual = await aces(target);
      if (actual !== expected) {
        failed.push(
          `${target}\n    expected ${expected}\n    actual   ${actual}`,
        );
      }
    }
    if (failed.length > 0) {
      throw new Error(
        `ACL differs from Program Files:\n  ${failed.join('\n  ')}`,
      );
    }
    console.log(
      chalk.green('✓ Installed entries inherit the Program Files ACL'),
    );
  } catch (error) {
    console.error(chalk.red('Test failed:'), error.message);
    process.exitCode = 1;
  } finally {
    await cleanupTestDir(testDir);
    await cleanupTestDir(controlDir);
    for (const candidate of stagingCandidates(testDir)) {
      await cleanupTestDir(candidate);
    }
  }
}

test();
