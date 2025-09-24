import { usePowerShell, $ } from 'zx';
import 'zx/globals';
import crypto from 'crypto';
import path from 'path';
import fs from 'fs-extra';
import { dev } from './utils.mjs';

usePowerShell();
$.verbose = true;

const FIXTURES_DIR = path.resolve('./fixtures');
const V1_DIR = path.join(FIXTURES_DIR, 'test-app-v1');
const V2_DIR = path.join(FIXTURES_DIR, 'test-app-v2');

async function main() {
  console.log(chalk.blue('Preparing test fixtures...'));

  // 清理并创建目录
  await fs.emptyDir(FIXTURES_DIR);
  await fs.ensureDir(V1_DIR);
  await fs.ensureDir(V2_DIR);

  // 创建测试应用文件
  await createAppFiles();

  // 创建配置文件
  await createConfig();

  // 构建完整的安装包流程
  await buildCompletePackages();

  console.log(chalk.green('✓ Test fixtures prepared successfully'));
}

const rnd_100k_1 = crypto.randomBytes(1024 * 300);
const rnd_100k_2 = crypto.randomBytes(1024 * 300);
const rnd_100k_3 = crypto.randomBytes(1024 * 800);
const rnd_100k_4 = crypto.randomBytes(1024 * 1000);
const rnd_100k_5 = crypto.randomBytes(1024 * 1000);
const rnd_100k_6 = crypto.randomBytes(1024 * 500);

async function createAppFiles() {
  // === V1 文件 ===
  // app.exe - 主程序
  const appExeV1 = Buffer.concat([
    Buffer.from('MZ'), // PE签名
    Buffer.from('APP_V1'), // 版本标记
    rnd_100k_1,
    rnd_100k_2,
    rnd_100k_3,
    rnd_100k_4,
    rnd_100k_5,
  ]);
  await fs.writeFile(path.join(V1_DIR, 'app.exe'), appExeV1);

  // config.json - 应用配置
  await fs.writeJSON(
    path.join(V1_DIR, 'config.json'),
    {
      version: '1.0.0',
      features: ['basic'],
    },
    { spaces: 2 },
  );

  // readme.txt - v1独有文件
  await fs.writeFile(
    path.join(V1_DIR, 'readme.txt'),
    'Test Application v1.0.0\\n',
  );

  // data/assets.dat - 数据文件
  await fs.ensureDir(path.join(V1_DIR, 'data'));
  await fs.writeFile(
    path.join(V1_DIR, 'data/assets.dat'),
    crypto.randomBytes(1024 * 10),
  );

  // === V2 文件 ===
  // app.exe - 更新的主程序
  const appExeV2 = Buffer.concat([
    Buffer.from('MZ'),
    Buffer.from('APP_V2'), // 版本标记
    rnd_100k_1, // 保持部分内容相同
    rnd_100k_2,
    rnd_100k_6,
    rnd_100k_4,
    rnd_100k_5, // 其余内容更新
  ]);
  await fs.writeFile(path.join(V2_DIR, 'app.exe'), appExeV2);

  // config.json - 更新的配置
  await fs.writeJSON(
    path.join(V2_DIR, 'config.json'),
    {
      version: '2.0.0',
      features: ['basic', 'advanced'],
    },
    { spaces: 2 },
  );

  // feature.dll - v2新增文件
  await fs.writeFile(
    path.join(V2_DIR, 'feature.dll'),
    crypto.randomBytes(1024 * 30),
  );

  // data/assets.dat - 更新的数据文件
  await fs.ensureDir(path.join(V2_DIR, 'data'));
  await fs.writeFile(
    path.join(V2_DIR, 'data/assets.dat'),
    crypto.randomBytes(1024 * 15),
  );

  // data/new-assets.dat - v2新增数据
  await fs.writeFile(
    path.join(V2_DIR, 'data/new-assets.dat'),
    crypto.randomBytes(1024 * 5),
  );

  console.log(chalk.gray('  App files created'));
}

async function createConfig() {
  // 根据README格式创建正确的配置文件
  const config = {
    source: [
      {
        id: 'local-v1',
        name: 'Local v1',
        uri: 'http://localhost:8080/test-app-v1.exe',
      },
      {
        id: 'local-v2',
        name: 'Local v2',
        uri: 'http://localhost:8080/test-app-v2.exe',
      },
    ],
    appName: 'Test Application',
    publisher: 'Test Publisher',
    regName: 'TestApp',
    exeName: 'app.exe',
    uninstallName: 'uninstall.exe',
    updaterName: 'updater.exe',
    programFilesPath: 'TestApp',
    title: 'Test Application',
    description: 'Integration test application',
    windowTitle: 'Test Application Installer',
    uacStrategy: 'prefer-user',
  };

  // v1和v2使用相同配置
  await fs.writeJSON(path.join(FIXTURES_DIR, 'kachina.config.json'), config, {
    spaces: 2,
  });
  config['__v2_patch'] = Date.now();
  await fs.writeJSON(
    path.join(FIXTURES_DIR, 'kachina.config.v2.json'),
    config,
    {
      spaces: 2,
    },
  );
  console.log(chalk.gray('  Config files created'));
}

async function buildCompletePackages() {
  if (dev) {
    // merge kachina-builder.exe+kachina-installer.exe to kachina-builder-bundle.exe
    console.log(chalk.gray('  Merging kachina-builder for dev...'));
    const builderExe = path.join(
      '..',
      'src-tauri',
      'target',
      'debug',
      'kachina-builder.exe',
    );
    const installerExe = path.join(
      '..',
      'src-tauri',
      'target',
      'debug',
      'kachina-installer.exe',
    );
    const mergedExe = path.join(
      '..',
      'src-tauri',
      'target',
      'debug',
      'kachina-builder-bundle.exe',
    );
    if (!(await fs.pathExists(builderExe))) {
      throw new Error(
        `kachina-builder not found at ${builderExe}. Please build it first.`,
      );
    }
    if (!(await fs.pathExists(installerExe))) {
      throw new Error(
        `kachina-installer not found at ${installerExe}. Please build it first.`,
      );
    }
    const builderData = await fs.readFile(builderExe);
    const installerData = await fs.readFile(installerExe);
    await fs.writeFile(mergedExe, Buffer.concat([builderData, installerData]));
    console.log(chalk.gray('  Merged kachina-builder for dev'));
  }
  const builderPath = path.join(
    '..',
    'src-tauri',
    'target',
    dev ? 'debug' : 'release',
    dev ? 'kachina-builder-bundle.exe' : 'kachina-builder.exe',
  );

  // 检查builder是否存在
  if (!(await fs.pathExists(builderPath))) {
    throw new Error(
      `kachina-builder not found at ${builderPath}. Please build it first.`,
    );
  }

  // 按照README的正确流程构建

  // === V1 构建流程 ===
  console.log(chalk.gray('  Building v1 updater...'));
  // 步骤2: 构建v1更新器
  await $`& ${builderPath} pack -c ${path.join(FIXTURES_DIR, 'kachina.config.json')} -o ${path.join(V1_DIR, 'updater.exe')}`;

  console.log(chalk.gray('  Generating v1 metadata...'));
  // 步骤3: 生成v1 metadata
  await $`& ${builderPath} gen -j 2 -i ${V1_DIR} -m ${path.join(FIXTURES_DIR, 'v1-metadata.json')} -o ${path.join(FIXTURES_DIR, 'v1-hashed')} -r TestApp -t 1.0.0 -u ${path.join(V1_DIR, 'updater.exe')}`;

  console.log(chalk.gray('  Building v1 offline package...'));
  // 步骤4: 构建v1离线包
  await $`& ${builderPath} pack -c ${path.join(FIXTURES_DIR, 'kachina.config.json')} -m ${path.join(FIXTURES_DIR, 'v1-metadata.json')} -d ${path.join(FIXTURES_DIR, 'v1-hashed')} -o ${path.join(FIXTURES_DIR, 'test-app-v1.exe')}`;

  // === V2 构建流程 ===
  console.log(chalk.gray('  Building v2 updater...'));
  // 步骤2: 构建v2更新器
  await $`& ${builderPath} pack -c ${path.join(FIXTURES_DIR, 'kachina.config.v2.json')} -o ${path.join(V2_DIR, 'updater.exe')}`;

  console.log(chalk.gray('  Generating v2 metadata...'));
  // 步骤3: 生成v2 metadata
  await $`& ${builderPath} gen -j 2 -i ${V2_DIR} -m ${path.join(FIXTURES_DIR, 'v2-metadata.json')} -d ${V1_DIR} -o ${path.join(FIXTURES_DIR, 'v2-hashed')} -r TestApp -t 2.0.0 -u ${path.join(V2_DIR, 'updater.exe')}`;

  console.log(chalk.gray('  Building v2 offline package...'));
  // 步骤4: 构建v2离线包
  await $`& ${builderPath} pack -c ${path.join(FIXTURES_DIR, 'kachina.config.v2.json')} -m ${path.join(FIXTURES_DIR, 'v2-metadata.json')} -d ${path.join(FIXTURES_DIR, 'v2-hashed')} -o ${path.join(FIXTURES_DIR, 'test-app-v2.exe')}`;

  console.log(chalk.gray('  All packages built'));
}

main().catch(console.error);
