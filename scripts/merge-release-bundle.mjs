import fs from 'fs/promises';
import path from 'path';

const releaseDir = path.resolve(
  'target',
  'x86_64-win7-windows-msvc',
  'release',
);
// 输入是 cargo 产物且永不改写；独立 builder 与最终 bundle 各用固定路径。
// 因此重复执行本脚本总是从原始 builder 重新生成，bundle 不会叠加 installer。
const builder = path.join(releaseDir, 'kachina-builder.exe');
const standalone = path.join(releaseDir, 'kachina-builder-standalone.exe');
const bundle = path.join(releaseDir, 'kachina-builder-bundle.exe');
const installer = path.join(releaseDir, 'kachina-installer.exe');

const [builderBytes, installerBytes] = await Promise.all([
  fs.readFile(builder),
  fs.readFile(installer),
]);
await fs.copyFile(builder, standalone);
await fs.writeFile(bundle, Buffer.concat([builderBytes, installerBytes]));

console.log(`Created ${bundle} (and ${standalone}) from builder and installer images`);
