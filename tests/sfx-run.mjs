import { spawn, spawnSync } from 'child_process';
import { dev, getTestDir, cleanupTestDir } from './utils.mjs';
import 'zx/globals';
import { usePowerShell, usePwsh } from 'zx';
// pwsh 以 Microsoft Store 别名安装时 node 侧可能探测不到，回退 Windows PowerShell
try {
  usePwsh();
} catch {
  usePowerShell();
}

// 自解压运行器 e2e：自建可运行应用（cmd.exe 副本充当 app.exe），走 gen + pack +
// replace-bin --base 产出 sfx 包，验证解压结果、cwd 继承、参数与退出码透传、sfx 不写 stdout、
// 临时目录清理与 reaper 行为。fixture 的 app.exe 是假 PE 不可运行，所以本测试不复用
// prepare.mjs 的产物。

const profileDir = dev
  ? path.resolve('..', 'target', 'debug')
  : path.resolve('..', 'target', 'x86_64-win7-windows-msvc', 'release');
const builderPath = path.join(profileDir, 'kachina-builder-bundle.exe');
const sfxPath = path.join(profileDir, 'kachina-sfx.exe');

async function runBuilder(args, label) {
  const result = await $({ nothrow: true })`& ${builderPath} ${args}`;
  if (result.exitCode !== 0) {
    throw new Error(label + ' failed (' + result.exitCode + '): ' + result.stderr);
  }
  return result;
}

// release 的 kachina-sfx 是 GUI 子系统程序，经 shell 的 `& exe` 调用时 shell 不等它退出；
// 直接拉起进程才能拿到退出码。
function runSfx(exe, args, label) {
  const result = spawnSync(exe, args, { encoding: 'utf8', windowsHide: true });
  if (result.error) {
    throw result.error;
  }
  console.log(chalk.gray('  ' + label + ': exit=' + result.status));
  return { exitCode: result.status, stdout: result.stdout, stderr: result.stderr };
}

async function waitFor(check, label, timeoutMs = 15000) {
  const deadline = Date.now() + timeoutMs;
  while (!check()) {
    if (Date.now() > deadline) {
      throw new Error('timed out waiting for ' + label);
    }
    await new Promise((r) => setTimeout(r, 100));
  }
}

function sfxTempDirs() {
  return fs
    .readdirSync(os.tmpdir())
    .filter((n) => n.startsWith('kachina-sfx-'))
    .map((n) => path.join(os.tmpdir(), n));
}

async function test() {
  const testDir = getTestDir('sfx-run');
  const appDir = path.join(testDir, 'app');
  const configPath = path.join(testDir, 'kachina.config.json');
  const metaPath = path.join(testDir, 'metadata.json');
  const hashedDir = path.join(testDir, 'hashed');
  const updaterPath = path.join(testDir, 'updater.exe');
  const installerPath = path.join(testDir, 'installer.exe');
  const portablePath = path.join(testDir, 'portable.exe');
  const onlineInstaller = path.join(testDir, 'online-only.exe');
  const onlinePortable = path.join(testDir, 'online-portable.exe');
  const iconPath = path.resolve('..', 'resources', 'icons', 'icon.ico');

  console.log(chalk.blue('=== SFX portable runner Test ==='));
  console.log('Builder: ' + builderPath);
  console.log('SFX base: ' + sfxPath);

  for (const [p, label] of [
    [builderPath, 'kachina-builder-bundle'],
    [sfxPath, 'kachina-sfx'],
  ]) {
    if (!(await fs.pathExists(p))) {
      throw new Error(label + ' not found at ' + p + '. Please build it first.');
    }
  }
  await fs.ensureDir(appDir);

  // cmd.exe 副本充当可运行的 app.exe：/c exit N 验证参数与退出码透传
  await fs.copy(
    path.join(process.env.SystemRoot || 'C:\\Windows', 'System32', 'cmd.exe'),
    path.join(appDir, 'app.exe'),
  );
  await fs.writeFile(path.join(appDir, 'data.txt'), 'SFX-DATA');

  await fs.writeJSON(
    configPath,
    {
      source: 'packed+http://localhost:8080/unused.exe',
      appName: 'Sfx Test App',
      publisher: 'Test Publisher',
      regName: 'SfxTestApp',
      exeName: 'app.exe',
      uninstallName: 'uninstall.exe',
      updaterName: 'updater.exe',
      programFilesPath: 'SfxTestApp',
      title: 'Sfx Test App',
      description: 'sfx e2e',
      windowTitle: 'Sfx Test',
      uacStrategy: 'prefer-user',
    },
    { spaces: 2 },
  );

  console.log('Building offline package...');
  await runBuilder(['pack', '-c', configPath, '--icon', iconPath, '-o', updaterPath], 'pack updater');
  await runBuilder(
    ['gen', '-j', '2', '-i', appDir, '-m', metaPath, '-o', hashedDir, '-r', 'SfxTestApp', '-t', '1.0.0', '-u', updaterPath],
    'gen',
  );
  await runBuilder(
    ['pack', '-c', configPath, '-m', metaPath, '-d', hashedDir, '--icon', iconPath, '-o', installerPath],
    'pack offline',
  );

  console.log('Swapping base to kachina-sfx...');
  await runBuilder(
    ['replace-bin', installerPath, '--base', sfxPath, '--icon', iconPath, '-o', portablePath],
    'replace-bin',
  );
  // 换上的 base 写入包内配置的窗口标题与应用名
  const info = await $`(Get-Item -LiteralPath ${portablePath}).VersionInfo | ConvertTo-Json -Compress`;
  const { FileDescription, ProductName } = JSON.parse(info.stdout);
  if (FileDescription !== 'Sfx Test' || ProductName !== 'Sfx Test App') {
    throw new Error(
      'portable version info: FileDescription=' + FileDescription + ' ProductName=' + ProductName,
    );
  }

  const before = new Set(sfxTempDirs());

  // app.exe（cmd）先在调用者的 cwd 写下 cwd，停留几秒再以 42 退出；
  // 停留期间核对解压结果，并验证另一次启动的 reaper 不删运行中的目录。
  console.log('Running portable package...');
  const cwdFile = path.join(testDir, 'cwd.txt');
  const first = spawn(
    portablePath,
    ['/c', 'cd > cwd.txt & ping -n 6 127.0.0.1 > nul & exit 42'],
    { cwd: testDir, windowsHide: true },
  );
  const firstExit = new Promise((resolve, reject) => {
    first.on('exit', resolve);
    first.on('error', reject);
  });
  await waitFor(() => fs.existsSync(cwdFile), 'app.exe started');
  const runDir = sfxTempDirs().find((d) => !before.has(d));
  if (!runDir) {
    throw new Error('no temp dir for the running app');
  }
  const data = await fs.readFile(path.join(runDir, 'data.txt'), 'utf8');
  if (data !== 'SFX-DATA') {
    throw new Error('unexpected data.txt content: ' + data);
  }
  console.log(chalk.gray('  data.txt extracted next to app.exe'));

  await $`(Get-Item -LiteralPath ${runDir}).CreationTime = (Get-Date).AddDays(-2)`;
  const second = runSfx(portablePath, ['/c', 'exit 0'], 'reaper with a running instance');
  if (second.exitCode !== 0) {
    throw new Error('second run failed: ' + second.stderr);
  }
  if (second.stdout !== '') {
    throw new Error('sfx wrote to stdout: ' + JSON.stringify(second.stdout));
  }
  if (!(await fs.pathExists(path.join(runDir, 'data.txt')))) {
    throw new Error('reaper removed the directory of a running app');
  }
  console.log(chalk.gray('  reaper kept the directory of a running app'));

  const code = await firstExit;
  if (code !== 42) {
    throw new Error('expected exit code 42, got ' + code);
  }
  const cwd = (await fs.readFile(cwdFile, 'utf8')).trim();
  if (cwd.toLowerCase() !== testDir.toLowerCase()) {
    throw new Error('app cwd ' + cwd + ' differs from caller cwd ' + testDir);
  }
  console.log(chalk.gray('  exit code and caller cwd passed through'));

  const leftovers = sfxTempDirs().filter((d) => !before.has(d));
  if (leftovers.length > 0) {
    throw new Error('temp dirs not cleaned: ' + leftovers.join(', '));
  }
  console.log(chalk.gray('  temp dirs cleaned after app exit'));

  // 双击路径（显示进度框）：没有可接的父控制台、输出也没重定向。node 拉起的子进程总能接上
  // node 的控制台，所以经 WMI 拉起，父进程是没有控制台的 WMI 服务进程；应用写下标记文件即通过。
  console.log('Running without a console...');
  const dialogMarker = path.join(testDir, 'dialog.txt');
  const commandLine = '"' + portablePath + '" /c "echo ok>dialog.txt"';
  const created = await $`$r = Invoke-CimMethod Win32_Process -MethodName Create -Arguments @{ CommandLine = ${commandLine}; CurrentDirectory = ${testDir} }; "$($r.ReturnValue) $($r.ProcessId)"`;
  const [createStatus, dialogPid] = created.stdout.trim().split(' ');
  if (createStatus !== '0') {
    throw new Error('Win32_Process.Create returned ' + createStatus);
  }
  try {
    await waitFor(() => fs.existsSync(dialogMarker), 'app started from the dialog path', 30000);
    await waitFor(
      () => sfxTempDirs().filter((d) => !before.has(d)).length === 0,
      'temp dir cleanup after the dialog path',
      30000,
    );
  } catch (error) {
    // 失败时 sfx 停在错误框上不会自己退出
    spawnSync('taskkill', ['/F', '/T', '/PID', dialogPid]);
    throw error;
  }
  console.log(chalk.gray('  dialog path extracted and ran the app'));

  console.log('Testing reaper...');
  const stamp = Date.now();
  const staleDir = path.join(os.tmpdir(), 'kachina-sfx-stale-' + stamp);
  const freshDir = path.join(os.tmpdir(), 'kachina-sfx-fresh-' + stamp);
  await fs.ensureDir(staleDir);
  await fs.writeFile(path.join(staleDir, 'leftover.bin'), 'x');
  await fs.ensureDir(freshDir);
  // reaper 只删超过年龄阈值的目录：把 stale 的创建时间拨到两天前
  await $`(Get-Item -LiteralPath ${staleDir}).CreationTime = (Get-Date).AddDays(-2)`;
  const run2 = runSfx(portablePath, ['/c', 'exit 0'], 'reaper run');
  if (run2.exitCode !== 0) {
    throw new Error('reaper run failed: ' + run2.stderr);
  }
  if (await fs.pathExists(staleDir)) {
    throw new Error('reaper did not remove stale dir');
  }
  if (!(await fs.pathExists(freshDir))) {
    throw new Error('reaper removed a fresh dir');
  }
  await fs.remove(freshDir);
  console.log(chalk.gray('  reaper removed stale dir, kept fresh dir'));

  console.log('Testing online-only package rejection...');
  await runBuilder(['pack', '-c', configPath, '--icon', iconPath, '-o', onlineInstaller], 'pack config-only');
  await runBuilder(['replace-bin', onlineInstaller, '--base', sfxPath, '-o', onlinePortable], 'replace-bin config-only');
  const run3 = runSfx(onlinePortable, [], 'online-only rejection');
  if (run3.exitCode !== 1) {
    throw new Error('expected exit code 1 for online-only package, got ' + run3.exitCode);
  }
  if (!run3.stderr.includes('离线数据')) {
    throw new Error('expected offline-data error on stderr, got: ' + run3.stderr);
  }
  console.log(chalk.gray('  online-only package rejected with clear error'));

  await cleanupTestDir(testDir);
  console.log(chalk.green('✓ SFX test passed'));
}

test().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
