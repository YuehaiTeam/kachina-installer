<template>
  <div class="main">
    <div v-show="!init" class="init-loading">
      <span class="fui-Spinner__spinner">
        <span class="fui-Spinner__spinnerTail"></span>
      </span>
    </div>
    <div v-show="init" class="content">
      <div class="image">
        <img src="./left.webp" alt="BetterGI" />
      </div>
      <div class="right">
        <div class="title">{{ PROJECT_CONFIG.title }}</div>
        <div class="desc">{{ PROJECT_CONFIG.description }}</div>
        <div v-if="step === 1" class="actions">
          <div v-if="!isUpdate" class="lnk">
            <Checkbox v-model="createLnk" />
            创建桌面快捷方式
          </div>
          <div v-if="!isUpdate" class="read">
            <Checkbox v-model="acceptEula" />
            我已阅读并同意
            <a> 用户协议 </a>
          </div>
          <div class="more">
            <span v-if="!isUpdate">安装到</span>
            <span v-if="isUpdate">更新到</span>
            <a @click="changeSource">{{ source }}</a>
          </div>
          <button
            class="btn btn-install"
            @click="install"
            :disabled="!isUpdate && !acceptEula"
          >
            {{ isUpdate ? '更新' : '安装' }}
          </button>
        </div>
        <div class="progress" v-if="step === 2">
          <div class="step-desc">
            <div
              v-for="(i, a) in subStepList"
              class="substep"
              :class="{ done: a < subStep }"
              v-show="a <= subStep"
              :key="i"
            >
              <span v-if="a === subStep" class="fui-Spinner__spinner">
                <span class="fui-Spinner__spinnerTail"></span>
              </span>
              <span v-else class="substep-done">
                <CircleSuccess />
              </span>
              <div>{{ i }}</div>
            </div>
          </div>
          <div class="current-status" v-html="current"></div>
          <div class="progress-bar" :style="{ width: `${percent}%` }"></div>
        </div>
        <div class="finish" v-if="step === 3">
          <div class="finish-text">
            <CircleSuccess />
            {{ isUpdate ? '更新' : '安装' }}完成
          </div>
          <button class="btn btn-install" @click="launch">启动</button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.main {
  min-height: 100vh;
}
.init-loading {
  height: 100vh;
  display: flex;
  justify-content: center;
  align-items: center;
  padding-bottom: 24px;
  box-sizing: border-box;
}

.init-loading .fui-Spinner__spinner {
  width: 40px;
  height: 40px;
  --fui-Spinner--strokeWidth: 4px;
}
.content {
  display: flex;
  min-height: 100vh;
  line-height: 1.1;
  text-align: center;
  justify-content: center;
  user-select: none;
  padding: 0 16px;
  gap: 8px;
}

.desc {
  font-size: 14px;
  opacity: 0.8;
  padding-left: 10px;
  padding-bottom: 2px;
}

.image {
  min-width: 180px;
  width: 180px;
  box-sizing: border-box;
  padding: 12px 0 12px 12px;

  img {
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
}

.right {
  position: relative;
  width: calc(100% - 188px);
  text-align: left;
  display: flex;
  flex-direction: column;
  padding: 16px;
  box-sizing: border-box;
  overflow: hidden;
}

.title {
  font-size: 25px;
  padding: 2px 10px 6px;
}

.btn-install {
  height: 40px;
  width: 140px;
  position: absolute;
  bottom: 20px;
  right: 24px;
}

.actions {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding-top: 16px;
}

.read,
.lnk {
  align-items: center;
  gap: 4px;
  padding-left: 12px;
  font-size: 13px;
  display: flex;

  a {
    cursor: pointer;
  }
}

.more {
  align-items: flex-start;
  gap: 6px;
  padding-top: 8px;
  padding-left: 10px;
  font-size: 13px;
  display: flex;
  flex-direction: column;

  span {
    opacity: 0.8;
  }

  a {
    cursor: pointer;
    font-family:
      Consolas,
      'Courier New',
      Microsoft Yahei;
    opacity: 0.8;
    font-size: 12px;
  }
}

.finish-text {
  text-align: center;
  opacity: 0.9;
  width: 100%;
  padding: 38px 10px;
  font-size: 18px;
  display: flex;
  justify-content: center;
  gap: 8px;
  align-items: center;

  svg {
    width: 24px;
  }
}

.progress-bar {
  position: fixed;
  bottom: 0;
  left: 0;
  height: 4px;
  background: var(--colorBrandForeground1);
  transition: width 0.1s;
  transition-timing-function: cubic-bezier(0.33, 0, 0.67, 1); /* easeInOut */
  width: 30%;
}

.step-desc {
  padding: 14px 10px;
  font-size: 14px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.substep {
  display: flex;
  gap: 6px;

  .fui-Spinner__spinner {
    width: 16px;
    height: 16px;
    display: block;
  }

  .substep-done {
    width: 16px;
    height: 16px;
    display: block;
  }
}

.substep.done {
  font-size: 13px;
  opacity: 0.8;
}

.current-status {
  position: relative;
  max-width: 100%;
  font-size: 12px;
  opacity: 0.7;
  padding-left: 14px;
  margin-top: -6px;
  font-family:
    Consolas,
    'Courier New',
    Microsoft Yahei;
}
</style>
<style>
.d-single-stat {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.d-single-list {
  display: flex;
  flex-direction: column;
  height: 55px;
  overflow: hidden;
  padding-top: 4px;
  font-size: 11px;
  gap: 2px;
  width: 230px;
  max-height: 250px;
  overflow-y: auto;
  padding-left: 20px;

  &::-webkit-scrollbar {
    width: 4px;
  }

  &::-webkit-scrollbar-thumb {
    background: var(--colorBrandForeground1);
    border-radius: 4px;
  }

  &::-webkit-scrollbar-track {
    background: var(--colorBrandBackground);
  }

  &::-webkit-scrollbar-thumb:hover {
    background: var(--colorBrandForeground2);
  }
}

.d-single {
  display: flex;
  justify-content: space-between;
  gap: 8px;
}

.d-single-filename {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
}

.d-single-progress {
  width: 36px;
  min-width: 36px;
}
</style>
<script lang="ts" setup>
import { onMounted, ref } from 'vue';
import { v4 as uuid } from 'uuid';
import { mapLimit } from 'async';
import Checkbox from './Checkbox.vue';
import CircleSuccess from './CircleSuccess.vue';
import { getCurrentWindow, invoke, listen, sep } from './tauri';
import { runDfsDownload } from './dfs';

const init = ref(false);

const subStepList: ReadonlyArray<string> = [
  '获取最新版本信息',
  '检查更新内容',
  '下载并安装',
];

const isUpdate = ref<boolean>(false);
const acceptEula = ref<boolean>(true);
const createLnk = ref<boolean>(true);
const step = ref<number>(1);
const subStep = ref<number>(0);

const current = ref<string>('');
const percent = ref<number>(0);
const source = ref<string>('');
const progressInterval = ref<number>(0);

const PROJECT_CONFIG: ProjectConfig = {
  dfsPath: 'bgi',
  appName: 'BetterGI',
  publisher: 'babalae',
  regName: 'BetterGI',
  exeName: 'BetterGI.exe',
  uninstallName: 'BetterGI.uninst.exe',
  updaterName: 'BetterGI.update.exe',
  programFilesPath: 'BetterGI\\BetterGI',
  title: 'BetterGI',
  description: '更好的原神，免费且开源',
  windowTitle: 'BetterGI 安装程序',
};

const INSTALLER_CONFIG: InstallerConfig = {
  install_path: '',
  install_path_exists: false,
  is_uninstall: false,
  embedded_config: null,
  enbedded_metadata: null,
  embedded_files: [],
  exe_path: '',
};

async function getSource(): Promise<InstallerConfig> {
  return await invoke<InstallerConfig>('get_installer_config', PROJECT_CONFIG);
}

async function runInstall(): Promise<void> {
  step.value = 2;
  let latest_meta = INSTALLER_CONFIG.enbedded_metadata;
  const online_meta = await invoke<InvokeGetDfsMetadataRes>(
    'get_dfs_metadata',
    { prefix: `${PROJECT_CONFIG.dfsPath}` },
  );
  if (!latest_meta) {
    latest_meta = online_meta;
    console.log('Local meta not found, use online meta');
  } else if (online_meta.tag_name !== latest_meta.tag_name) {
    console.log('Version update detected, use online meta');
    latest_meta = online_meta;
  } else {
    console.log('Local meta found, use local meta');
  }
  let hashKey = '';
  if (latest_meta.hashed.every((e) => e.md5)) {
    hashKey = 'md5';
  } else if (latest_meta.hashed.every((e) => e.xxh)) {
    hashKey = 'xxh';
  } else {
    throw new Error('更新服务端配置有误，不支持的哈希算法');
  }
  subStep.value = 1;
  percent.value = 5;
  let id = uuid();
  let idUnListen = await listen<[number, number]>(id, ({ payload }) => {
    const [currentValue, total] = payload;
    current.value = `${currentValue} / ${total}`;
    percent.value = 5 + (currentValue / total) * 15;
  });
  const local_meta = (
    await invoke<InvokeDeepReaddirWithMetadataRes>(
      'deep_readdir_with_metadata',
      { id, source: source.value, hashAlgorithm: hashKey },
    )
  ).map((e) => {
    return {
      ...e,
      file_name: e.file_name.replace(source.value, ''),
    };
  });
  idUnListen();
  current.value = '校验本地文件……';
  const diff_files: Array<DfsUpdateTask> = [];
  const strip_first_slash = (s: string) => {
    let ss = s.replace(/\\/g, '/');
    if (ss.startsWith('/')) return ss.slice(1);
    return ss;
  };
  for (const item of latest_meta.hashed) {
    const local = local_meta.find(
      (e) =>
        strip_first_slash(e.file_name) === strip_first_slash(item.file_name),
    );
    if (!local || local.hash !== item[hashKey as DfsMetadataHashType]) {
      let patch = latest_meta.patches?.find(
        (e) =>
          e.from[hashKey as DfsMetadataHashType] ===
          item[hashKey as DfsMetadataHashType],
      );
      let lpatch = latest_meta.patches?.find((e) =>
        INSTALLER_CONFIG.embedded_files?.some(
          (em) => em.name === e.from[hashKey as DfsMetadataHashType],
        ),
      );
      diff_files.push({
        ...item,
        patch,
        lpatch,
        downloaded: 0,
        running: false,
      });
    }
  }
  if (diff_files.length === 0) {
    percent.value = 100;
    step.value = 3;
    await finishInstall(latest_meta);
    return;
  }
  subStep.value = 2;
  current.value = '准备下载……';
  const total_size = diff_files.reduce(
    (acc, cur) => acc + (cur?.patch?.size || cur.size),
    0,
  );
  let stat: InstallStat = {
    speedLastSize: 0,
    lastTime: performance.now(),
    speed: 0,
  };
  progressInterval.value = setInterval(() => {
    const now = performance.now();
    const time_diff = now - stat.lastTime;
    const downloadedTotalSize = diff_files.reduce(
      (acc, cur) => acc + cur.downloaded,
      0,
    );
    stat.speed = (downloadedTotalSize - stat.speedLastSize) / time_diff;
    stat.speedLastSize = downloadedTotalSize;
    stat.lastTime = now;
    const speed = formatSize(stat.speed * 1000);
    const downloaded = formatSize(downloadedTotalSize);
    const total = formatSize(total_size);
    const runningTasks = diff_files
      .filter((e) => e.running)
      .map((e) => `${basename(e.file_name)} ${formatSize(e.downloaded)}`);
    current.value = `
      <span class="d-single-stat">${downloaded} / ${total} (${speed}/s)</span>
      <div class="d-single-list">
        <div class="d-single">
          ${Object.values(runningTasks).join('</div><div class="d-single">')}
        </div>
      </div>
    `;
    percent.value = 20 + (downloadedTotalSize / total_size) * 80;
  }, 400);
  await mapLimit(diff_files, 5, async (item: (typeof diff_files)[0]) => {
    let hasError = false;
    for (let i = 0; i < 3; i++) {
      try {
        await runDfsDownload(
          INSTALLER_CONFIG.embedded_files || [],
          source.value,
          hashKey as DfsMetadataHashType,
          item,
          hasError,
          hasError,
        );
        break;
      } catch (e) {
        console.error(e);
        if (i === 2) {
          throw e;
        }
      }
    }
  });
  clearInterval(progressInterval.value);
  await finishInstall(latest_meta);
  current.value = '安装完成';
  step.value = 3;
  percent.value = 100;
}

async function finishInstall(
  latest_meta: InvokeGetDfsMetadataRes,
): Promise<void> {
  const [program, desktop] = await invoke<InvokeGetDirsRes>('get_dirs');
  if (createLnk.value && !isUpdate.value) {
    await invokeCreateLnk(
      `${source.value}${sep()}${PROJECT_CONFIG.exeName}`,
      `${desktop}${sep()}${PROJECT_CONFIG.appName}.lnk`,
    );
  }
  if (!isUpdate.value) {
    await invokeCreateLnk(
      `${source.value}${sep()}${PROJECT_CONFIG.exeName}`,
      `${program}${sep()}${PROJECT_CONFIG.appName}${sep()}${PROJECT_CONFIG.appName}.lnk`,
    ).catch(console.error);
    await invoke('create_uninstaller', {
      source: source.value,
      uninstallerName: PROJECT_CONFIG.uninstallName,
      updaterName: PROJECT_CONFIG.updaterName,
    }).catch(console.error);
    await invoke('write_registry', {
      regName: PROJECT_CONFIG.regName,
      name: PROJECT_CONFIG.appName,
      version: latest_meta.tag_name,
      exe: `${source.value}${sep()}${PROJECT_CONFIG.exeName}`,
      source: source.value,
      uninstaller: `${source.value}${sep()}${PROJECT_CONFIG.uninstallName}`,
      metadata: JSON.stringify(latest_meta),
      size: latest_meta.hashed.reduce((acc, cur) => acc + cur.size, 0),
      publisher: PROJECT_CONFIG.publisher,
    }).catch(console.error);
    await invokeCreateLnk(
      `${source.value}${sep()}${PROJECT_CONFIG.uninstallName}`,
      `${program}${sep()}${PROJECT_CONFIG.appName}${sep()}卸载${PROJECT_CONFIG.appName}.lnk`,
    ).catch(console.error);
  }
}

async function install(): Promise<void> {
  try {
    await runInstall();
  } catch (e) {
    console.error(e);
    if (e instanceof Error) await error(e.stack || e.toString());
    else await error(JSON.stringify(e));
    step.value = 1;
    subStep.value = 0;
    percent.value = 0;
    current.value = '';
    clearInterval(progressInterval.value);
    progressInterval.value = 0;
  }
}

onMounted(async () => {
  const win = getCurrentWindow();
  await win.show();
  const rsrc = await getSource();
  console.log('INSTALLER_CONFIG: ', rsrc);
  Object.assign(INSTALLER_CONFIG, rsrc);
  if (INSTALLER_CONFIG.embedded_config) {
    Object.assign(PROJECT_CONFIG, INSTALLER_CONFIG.embedded_config);
  }
  source.value = INSTALLER_CONFIG.install_path;
  if (INSTALLER_CONFIG.install_path_exists) isUpdate.value = true;
  await win.setTitle(PROJECT_CONFIG.windowTitle);
  init.value = true;
});

function formatSize(size: number): string {
  if (size < 1024) {
    return `${size.toFixed(2)} B`;
  }
  if (size < 1024 * 1024) {
    return `${(size / 1024).toFixed(2)} KB`;
  }
  return `${(size / 1024 / 1024).toFixed(2)} MB`;
}

function basename(path: string): string {
  return path.replace(/\\/g, '/').split('/').pop() as string;
}

async function launch() {
  // todo 这里是否可以替换成 PROJECT_CONFIG.exeName?
  const mainExe = `BetterGI.exe`;
  const fullPath = `${source.value}${sep()}${mainExe}`;
  await invoke('launch_and_exit', { path: fullPath });
}

async function changeSource() {
  try {
    const result = await invoke<InvokeSelectDirRes>('select_dir', {
      path: source.value,
    });
    if (result === null) return;
    const [isEmpty, hasExePath] = await invoke<boolean[]>('is_dir_empty', {
      path: result,
      ...PROJECT_CONFIG,
    });
    isUpdate.value = hasExePath;
    if (!isEmpty && !hasExePath) {
      await invoke('ensure_dir', {
        path: `${result}${sep()}${PROJECT_CONFIG.regName}`,
      });
      source.value = `${result}${sep()}${PROJECT_CONFIG.regName}`;
    } else {
      source.value = result;
    }
  } catch (e) {
    if (e instanceof Error) await error(e.stack || e.toString());
    else await error(JSON.stringify(e));
    throw e;
  }
}

async function invokeCreateLnk(target: string, lnk: string): Promise<void> {
  return await invoke('create_lnk', { target, lnk });
}

async function error(message: string, title = '出错了'): Promise<void> {
  await invoke('error_dialog', { message, title });
}
</script>
