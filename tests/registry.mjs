import {
  FLAGS,
  runInstaller,
  assertExitOk,
  clearLogFile,
  getFileHash,
  printLogFileIfExists,
  stagingCandidates,
  waitForServer,
  readUninstallRecord,
  writeUninstallRecord,
  removeUninstallRecord,
  moveUninstallRecord,
  removeUserShortcuts,
  normalizeInstallPath,
  getLogFilePath,
} from './utils.mjs';
import { startServer } from './server.mjs';
import 'zx/globals';
import { usePwsh } from 'zx';
usePwsh();

const REG_NAME = 'TestApp';
const APP_NAME = 'Test Application';
const INSTALLER_V1 = './fixtures/test-app-v1.exe';
const PORTABLE_V1 = './fixtures/test-app-v1';
const HIVES = ['HKCU', 'HKLM'];

// Hosted runners expose %TEMP% as an 8.3 short path; the installer records
// the path it is given, so compare against the long form.
const TMP = fs.realpathSync.native(os.tmpdir());
const dirs = [];

function testDir(name) {
  const dir = path.join(TMP, `kachina-test-registry-${name}-${Date.now()}`);
  dirs.push(dir);
  return dir;
}

async function loadMetadata(version) {
  return fs.readJSON(`./fixtures/${version}-metadata.json`);
}

/** The manifest an update from online metadata installs: the package's
 * `installer` image also lands as the updater. */
function withUpdater(metadata) {
  const { size, xxh, md5 } = metadata.installer;
  return {
    ...metadata,
    hashed: [...metadata.hashed, { file_name: 'updater.exe', size, xxh, md5 }],
  };
}

function manifestKeys(meta) {
  return (meta.hashed ?? [])
    .map((e) => `${e.file_name}|${e.xxh ?? e.md5}`)
    .sort()
    .join('\n');
}

class Check {
  constructor(label) {
    this.label = label;
    this.failed = [];
  }

  equal(where, expected, actual) {
    if (expected !== actual) {
      this.failed.push(
        `${where}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`,
      );
    }
  }

  ok(where, condition, detail) {
    if (!condition) {
      this.failed.push(`${where}: ${detail}`);
    }
  }

  finish() {
    if (this.failed.length > 0) {
      throw new Error(
        `${this.label}:\n${this.failed.map((m) => `  - ${m}`).join('\n')}`,
      );
    }
    console.log(chalk.green(`✓ ${this.label}`));
  }
}

async function expectRecord(check, hive, dir, metadata) {
  const record = await readUninstallRecord(hive, REG_NAME);
  if (!record) {
    check.failed.push(`${hive}: record missing`);
    return;
  }
  check.equal(
    `${hive}.DisplayVersion`,
    metadata.tag_name,
    record.DisplayVersion,
  );
  check.equal(
    `${hive}.InstallLocation`,
    normalizeInstallPath(dir),
    normalizeInstallPath(record.InstallLocation ?? ''),
  );
  const uninstaller = path.join(dir, 'uninstall.exe');
  check.equal(
    `${hive}.UninstallString`,
    normalizeInstallPath(uninstaller),
    normalizeInstallPath(record.UninstallString ?? ''),
  );
  check.ok(
    `${hive}.UninstallString`,
    await fs.pathExists(uninstaller),
    `${uninstaller} does not exist`,
  );
  let meta = null;
  try {
    meta = JSON.parse(record.InstallerMeta ?? '');
  } catch (error) {
    check.failed.push(`${hive}.InstallerMeta: not JSON (${error.message})`);
  }
  if (meta) {
    check.equal(
      `${hive}.InstallerMeta.tag_name`,
      metadata.tag_name,
      meta.tag_name,
    );
    check.equal(
      `${hive}.InstallerMeta.hashed`,
      manifestKeys(metadata),
      manifestKeys(meta),
    );
  }
  const size = metadata.hashed.reduce((sum, e) => sum + e.size, 0);
  check.equal(
    `${hive}.EstimatedSize`,
    Math.floor(size / 1024),
    record.EstimatedSize,
  );
}

async function expectAbsent(check, hive) {
  const record = await readUninstallRecord(hive, REG_NAME);
  check.ok(
    hive,
    record === null,
    `unexpected record ${JSON.stringify(record)}`,
  );
}

async function expectUnchanged(check, hive, before) {
  const record = await readUninstallRecord(hive, REG_NAME);
  check.equal(`${hive} record`, JSON.stringify(before), JSON.stringify(record));
}

async function expectAppVersion(check, dir, marker) {
  const app = path.join(dir, 'app.exe');
  const content = (await fs.pathExists(app)) ? await fs.readFile(app) : null;
  check.ok('app.exe', content?.includes(marker), `does not contain ${marker}`);
}

function foreignRecord(dir, version) {
  return {
    DisplayName: APP_NAME,
    DisplayVersion: version,
    InstallLocation: dir,
    UninstallString: path.join(dir, 'uninstall.exe'),
    InstallerMeta: JSON.stringify({ tag_name: version, hashed: [] }),
  };
}

/** Launch the updater inside `dir` the way a user double-clicks it: that
 * directory as the working directory and no `-D`. */
async function runUpdaterInDir(dir, label, extra = []) {
  await clearLogFile();
  const result = await runInstaller(
    path.join(dir, 'updater.exe'),
    [FLAGS, '--source', 'local-v2', ...extra],
    label,
    '3m',
    dir,
  );
  assertExitOk(result, label);
}

async function newInstall(dir, v1) {
  const check = new Check('New install writes one HKCU record');
  await clearLogFile();
  const result = await runInstaller(
    INSTALLER_V1,
    [FLAGS, '-D', dir],
    'Install',
  );
  assertExitOk(result, 'Install');
  await expectRecord(check, 'HKCU', dir, v1);
  await expectAbsent(check, 'HKLM');
  check.finish();
}

async function alreadyLatestRepair(dir, v1) {
  const check = new Check('Already-latest run repairs a stale record');
  await writeUninstallRecord('HKCU', REG_NAME, {
    DisplayVersion: '0.0.1',
    InstallerMeta: JSON.stringify({ tag_name: '0.0.1', hashed: [] }),
  });
  const appHash = await getFileHash(path.join(dir, 'app.exe'));
  await clearLogFile();
  const result = await runInstaller(
    INSTALLER_V1,
    [FLAGS, '-D', dir],
    'Already-latest run',
  );
  assertExitOk(result, 'Already-latest run');
  check.equal(
    'app.exe hash',
    appHash,
    await getFileHash(path.join(dir, 'app.exe')),
  );
  await expectRecord(check, 'HKCU', dir, v1);
  await expectAbsent(check, 'HKLM');
  check.finish();
}

async function registeredUpdate(dir, v2) {
  const check = new Check('Direct-launch update refreshes the matching record');
  await runUpdaterInDir(dir, 'Registered update');
  await expectAppVersion(check, dir, 'APP_V2');
  await expectRecord(check, 'HKCU', dir, withUpdater(v2));
  await expectAbsent(check, 'HKLM');
  check.finish();
}

async function uninstall(dir, otherDir) {
  const check = new Check('Uninstall removes only this directory’s record');
  await writeUninstallRecord(
    'HKLM',
    REG_NAME,
    foreignRecord(otherDir, '9.9.9'),
  );
  const foreign = await readUninstallRecord('HKLM', REG_NAME);
  await clearLogFile();
  const result = await runInstaller(
    path.join(dir, 'uninstall.exe'),
    [FLAGS],
    'Uninstall',
    '3m',
    dir,
  );
  assertExitOk(result, 'Uninstall');
  check.ok(
    'app.exe',
    !(await fs.pathExists(path.join(dir, 'app.exe'))),
    'still exists',
  );
  check.ok(
    'User/settings.json',
    await fs.pathExists(path.join(dir, 'User/settings.json')),
    'user data removed although deleteUserData defaults to false',
  );
  await expectAbsent(check, 'HKCU');
  await expectUnchanged(check, 'HKLM', foreign);
  await removeUninstallRecord('HKLM', REG_NAME);
  check.finish();
}

async function portableUpdate(dir) {
  const check = new Check('Portable update stays unregistered');
  await fs.copy(PORTABLE_V1, dir);
  await runUpdaterInDir(dir, 'Portable update');
  await expectAppVersion(check, dir, 'APP_V2');
  for (const hive of HIVES) {
    await expectAbsent(check, hive);
  }
  check.ok(
    'uninstall.exe',
    !(await fs.pathExists(path.join(dir, 'uninstall.exe'))),
    'created in a portable directory',
  );
  check.finish();
}

async function portableUpdateBesideOtherRecord(dir, otherDir) {
  const check = new Check(
    'Portable update leaves a record for another directory alone',
  );
  await fs.copy(PORTABLE_V1, dir);
  await writeUninstallRecord(
    'HKCU',
    REG_NAME,
    foreignRecord(otherDir, '1.0.0'),
  );
  const foreign = await readUninstallRecord('HKCU', REG_NAME);
  await runUpdaterInDir(dir, 'Portable update beside another record');
  await expectAppVersion(check, dir, 'APP_V2');
  await expectUnchanged(check, 'HKCU', foreign);
  await expectAbsent(check, 'HKLM');
  check.ok(
    'uninstall.exe',
    !(await fs.pathExists(path.join(dir, 'uninstall.exe'))),
    'created in a portable directory',
  );
  check.finish();
}

const HELPER = '--assume-unelevated';

async function expectHelperUsed(check) {
  const logFile = getLogFilePath();
  const log = (await fs.pathExists(logFile))
    ? await fs.readFile(logFile, 'utf-8')
    : '';
  check.ok(
    'elevation helper',
    log.includes('Elevate process started'),
    'operations did not go through the helper process',
  );
}

/** A matching HKLM record makes a standard user's run elevate; with the
 * helper flag the elevated runner takes the same helper path. */
async function machineUpdate(dir, v1, v2) {
  const check = new Check('Helper update rewrites the HKLM record');
  await clearLogFile();
  const result = await runInstaller(
    INSTALLER_V1,
    [FLAGS, '-D', dir],
    'Install',
  );
  assertExitOk(result, 'Install');
  await moveUninstallRecord('HKCU', 'HKLM', REG_NAME);
  await expectRecord(check, 'HKLM', dir, v1);
  await runUpdaterInDir(dir, 'Helper update', [HELPER]);
  await expectHelperUsed(check);
  await expectAppVersion(check, dir, 'APP_V2');
  await expectRecord(check, 'HKLM', dir, withUpdater(v2));
  await expectAbsent(check, 'HKCU');
  check.finish();
}

async function machineUninstall(dir) {
  const check = new Check('Helper uninstall removes the HKLM record');
  await clearLogFile();
  const result = await runInstaller(
    path.join(dir, 'uninstall.exe'),
    [FLAGS, HELPER],
    'Helper uninstall',
    '3m',
    dir,
  );
  assertExitOk(result, 'Helper uninstall');
  await expectHelperUsed(check);
  check.ok(
    'app.exe',
    !(await fs.pathExists(path.join(dir, 'app.exe'))),
    'still exists',
  );
  for (const hive of HIVES) {
    await expectAbsent(check, hive);
  }
  check.finish();
}

async function cleanup() {
  for (const hive of HIVES) {
    await removeUninstallRecord(hive, REG_NAME);
  }
  await removeUserShortcuts(APP_NAME);
  for (const dir of dirs) {
    for (const target of [dir, ...stagingCandidates(dir)]) {
      await fs.remove(target).catch((error) => {
        console.warn(
          chalk.yellow(`Failed to remove ${target}: ${error.message}`),
        );
      });
    }
  }
}

async function test() {
  console.log(chalk.blue('=== Registry Test ==='));
  const v1 = await loadMetadata('v1');
  const v2 = await loadMetadata('v2');
  let server;
  let failed = false;
  try {
    await cleanup();
    server = await startServer();
    await waitForServer('http://localhost:8080/test-app-v2.exe');

    const installed = testDir('installed');
    await newInstall(installed, v1);
    await alreadyLatestRepair(installed, v1);
    await registeredUpdate(installed, v2);
    await uninstall(installed, testDir('foreign-hklm'));
    await portableUpdate(testDir('portable'));
    await portableUpdateBesideOtherRecord(
      testDir('portable-beside'),
      testDir('foreign-hkcu'),
    );
    await cleanup();

    const machine = testDir('machine');
    await machineUpdate(machine, v1, v2);
    await machineUninstall(machine);
  } catch (error) {
    failed = true;
    console.error(chalk.red('Test failed:'), error.message);
    await printLogFileIfExists();
  } finally {
    server?.close();
    await cleanup();
  }
  process.exit(failed ? 1 : 0);
}

test();
