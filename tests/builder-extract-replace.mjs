import {
  verifyFiles,
  cleanupTestDir,
  getTestDir,
  FLAGS,
  runInstaller,
  assertExitOk,
} from './utils.mjs';
import 'zx/globals';
import { usePwsh } from 'zx';
usePwsh();

const builderPath = path.resolve(
  '..',
  'src-tauri',
  'target',
  'release',
  'kachina-builder.exe',
);

async function runBuilder(args, label) {
  const result = await $`& ${builderPath} ${args}`;
  assertExitOk(result, label);
  return result;
}

async function test() {
  const testDir = getTestDir('builder-extract-replace');
  const installerPath = path.resolve('./fixtures/test-app-v1.exe');
  const extractDir = path.join(testDir, 'extract-all');
  const configOut = path.join(testDir, 'config.json');
  const replaced = path.join(testDir, 'replaced.exe');
  const installDir = path.join(testDir, 'installed');

  console.log(chalk.blue('=== Builder extract / replace-bin Test ==='));
  console.log(`Builder: ${builderPath}`);
  console.log(`Package: ${installerPath}`);

  try {
    if (!(await fs.pathExists(builderPath))) {
      throw new Error(`kachina-builder not found at ${builderPath}`);
    }
    if (!(await fs.pathExists(installerPath))) {
      throw new Error(`fixture not found at ${installerPath}`);
    }
    await fs.ensureDir(testDir);

    console.log('Listing packed files...');
    const listed = await runBuilder(
      ['extract', '-i', installerPath, '--list'],
      'extract --list',
    );
    const listText = `${listed.stdout}\n${listed.stderr}`;
    if (!listText.includes('CONFIG') || !listText.includes('INDEX')) {
      throw new Error('extract --list missing CONFIG/INDEX');
    }
    if (!listText.includes('app.exe')) {
      throw new Error('extract --list missing metadata name app.exe');
    }

    console.log('Extracting all files...');
    await runBuilder(
      ['extract', '-i', installerPath, '--all', extractDir],
      'extract --all',
    );
    if (!(await fs.pathExists(path.join(extractDir, 'app.exe')))) {
      throw new Error('extract --all did not write app.exe');
    }
    if (!(await fs.pathExists(path.join(extractDir, 'config.json')))) {
      throw new Error('extract --all did not write config.json');
    }

    console.log('Extracting CONFIG by hash name...');
    await runBuilder(
      ['extract', '-i', installerPath, '-n', '\\0CONFIG', '-f', configOut],
      'extract --name CONFIG',
    );
    const config = await fs.readJSON(configOut);
    if (config.appName !== 'Test Application') {
      throw new Error(`unexpected extracted config appName: ${config.appName}`);
    }

    console.log('Replacing installer stub...');
    await runBuilder(
      ['replace-bin', installerPath, '-o', replaced],
      'replace-bin',
    );
    if (!(await fs.pathExists(replaced))) {
      throw new Error('replace-bin did not write output');
    }

    const originalSize = (await fs.stat(installerPath)).size;
    const replacedSize = (await fs.stat(replaced)).size;
    console.log(
      chalk.gray(
        `  original=${originalSize} replaced=${replacedSize} delta=${replacedSize - originalSize}`,
      ),
    );

    console.log('Listing replaced package...');
    const relisted = await runBuilder(
      ['extract', '-i', replaced, '--list'],
      'extract --list replaced',
    );
    const relistText = `${relisted.stdout}\n${relisted.stderr}`;
    if (!relistText.includes('app.exe') || !relistText.includes('INDEX')) {
      throw new Error('replaced package lost INDEX or app.exe');
    }

    console.log('Installing replaced package silently...');
    const result = await runInstaller(
      replaced,
      [FLAGS, '-D', installDir],
      'Replaced package install',
    );
    assertExitOk(result, 'Replaced package install');

    const verification = await verifyFiles(installDir, [
      { path: 'app.exe', contains: 'APP_V1' },
      { path: 'config.json', contains: '"version": "1.0.0"' },
      { path: 'readme.txt', contains: 'v1.0.0' },
      { path: 'updater.exe' },
    ]);
    if (verification.failed.length) {
      throw new Error(verification.failed.join('; '));
    }
    console.log(chalk.green('✓ extract / replace-bin kept a working package'));
    console.log(chalk.gray(`  Verified: ${verification.passed.join(', ')}`));
  } catch (error) {
    console.error(chalk.red('Test failed:'), error.message);
    process.exit(1);
  } finally {
    await cleanupTestDir(testDir);
  }
  process.exit(0);
}

test();
