import fs from 'fs/promises';
import path from 'path';

const releaseDir = path.resolve(
  'src-tauri',
  'target',
  'x86_64-win7-windows-msvc',
  'release',
);
const builder = path.join(releaseDir, 'kachina-builder.exe');
const standalone = path.join(releaseDir, 'kachina-builder-standalone.exe');
const installer = path.join(releaseDir, 'kachina-installer.exe');

const [builderBytes, installerBytes] = await Promise.all([
  fs.readFile(builder),
  fs.readFile(installer),
]);
await fs.rm(standalone, { force: true });
await fs.rename(builder, standalone);

await fs.writeFile(builder, Buffer.concat([builderBytes, installerBytes]));

console.log(`Created ${builder} from builder and installer images`);
