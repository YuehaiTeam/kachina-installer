<template>
  <div class="content">
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
</template>

<style scoped>
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
  width: 180px;
  padding: 12px;
  box-sizing: border-box;
  padding-right: 0;

  img {
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
}

.right {
  flex: 1;
  text-align: left;
  display: flex;
  flex-direction: column;
  padding: 16px;
}

.title {
  font-size: 25px;
  padding: 6px 10px;
  padding-top: 2px;
}

.checkbox {
  height: 16px;
  overflow: hidden;
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

  .checkbox {
    margin-right: 6px;
    margin-top: 1px;
  }

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
  font-size: 12px;
  opacity: 0.7;
  padding-left: 34px;
  margin-top: -6px;
  font-family:
    Consolas,
    'Courier New',
    Microsoft Yahei;
}
</style>
<style>
.d-single-list {
  display: flex;
  flex-direction: column;
  height: 55px;
  overflow: hidden;
  padding-top: 4px;
  font-size: 11px;
  gap: 2px;
  width: 230px;
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
const connectableOrigins = new Set();

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

async function getSource(): Promise<InvokeGetInstallSourceRes> {
  return await invoke<InvokeGetInstallSourceRes>(
    'get_install_source',
    PROJECT_CONFIG,
  );
}

async function runInstall(): Promise<void> {
  step.value = 2;
  const latest_meta = await invoke<InvokeGetDfsMetadataRes>(
    'get_dfs_metadata',
    { prefix: `${PROJECT_CONFIG.dfsPath}` },
  );
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
  const diff_files: Array<DfsMetadataHashInfo> = [];
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
      diff_files.push(item);
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
  const total_size = diff_files.reduce((acc, cur) => acc + cur.size, 0);
  let stat: InstallStat = {
    downloadedTotalSize: 0,
    speedLastSize: 0,
    lastTime: performance.now(),
    speed: 0,
    runningTasks: {},
  };
  progressInterval.value = setInterval(() => {
    const now = performance.now();
    const time_diff = now - stat.lastTime;
    stat.speed = (stat.downloadedTotalSize - stat.speedLastSize) / time_diff;
    stat.speedLastSize = stat.downloadedTotalSize;
    stat.lastTime = now;
    const speed = formatSize(stat.speed * 1000);
    const downloaded = formatSize(stat.downloadedTotalSize);
    const total = formatSize(total_size);
    current.value =
      `${downloaded} / ${total} (${speed}/s)<div class="d-single-list"><div class="d-single">` +
      Object.values(stat.runningTasks).join('</div><div class="d-single">') +
      '</div></div>';
    percent.value = 20 + (stat.downloadedTotalSize / total_size) * 80;
  }, 400);
  await mapLimit(diff_files, 5, async (item: (typeof diff_files)[0]) => {
    stat.runningTasks[item.file_name] =
      `<span class="d-single-filename">${basename(item.file_name)}</span><span class="d-single-progress">0%</span>`;
    const dfs_result = await invoke<InvokeGetDfsRes>('get_dfs', {
      path: `bgi/hashed/${item[hashKey as DfsMetadataHashType]}`,
    });
    const id = uuid();
    let last_downloaded_size = 0;
    let idUnListen = await listen<number>(id, ({ payload }) => {
      let current_size = payload;
      const size_diff = current_size - last_downloaded_size;
      last_downloaded_size = current_size;
      stat.downloadedTotalSize += size_diff;
      stat.runningTasks[item.file_name] =
        `<span class="d-single-filename">${basename(item.file_name)}</span><span class="d-single-progress">${Math.round(current_size / item.size)}%</span>`;
    });
    let filename_with_first_slash = item.file_name.startsWith('/')
      ? item.file_name
      : `/${item.file_name}`;
    let url = dfs_result.url;
    if (!url && (dfs_result.tests?.length || 0) > 0) {
      const tests = dfs_result.tests;
      if (tests.length > 0) {
        const now = performance.now();
        const result = await Promise.race(
          tests.map((test) => {
            const origin = new URL(test[0]).origin;
            if (connectableOrigins.has(origin)) {
              return { url: test[1], time: 10 };
            }
            return fetchWithTimeout(test[0], { method: 'HEAD' })
              .then((response) => {
                if (response.ok) {
                  connectableOrigins.add(origin);
                  return { url: test[1], time: performance.now() - now };
                }
                throw new Error('not ok');
              })
              .catch(() => ({ url: test[0], time: -1 }));
          }),
        );
        if (result.time > 0) url = result.url;
      }
    }
    if (!url && dfs_result.source) url = dfs_result.source;
    if (!url && dfs_result.tests?.length) url = dfs_result.tests[0][1];
    if (!url) {
      throw new Error('没有可用的下载节点：' + JSON.stringify(dfs_result));
    }
    console.log('dfs-url', url);
    const run_dl = async () => {
      try {
        await invoke('download_and_decompress', {
          id,
          url: url,
          target: source.value + filename_with_first_slash,
        });
      } catch (e) {
        console.error(e);
        stat.downloadedTotalSize -= last_downloaded_size;
        throw e;
      } finally {
        idUnListen();
        delete stat.runningTasks[item.file_name];
      }
      const size_diff = item.size - last_downloaded_size;
      stat.downloadedTotalSize += size_diff;
    };
    for (let i = 0; i < 3; i++) {
      try {
        await run_dl();
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
    if (e instanceof Error) await error(e.toString());
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
  const [sourcePath, sourceExists] = await getSource();
  source.value = sourcePath;
  if (sourceExists) isUpdate.value = true;
  const win = getCurrentWindow();
  await win.setTitle(PROJECT_CONFIG.windowTitle);
  await win.show();
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
  return path.replace(/\\/g, '/').split('/').pop();
}

function fetchWithTimeout(
  url: string,
  options: RequestInit,
  timeout = 2000,
): Promise<Response> {
  return Promise.race([
    fetch(url, options),
    new Promise((_, reject) =>
      setTimeout(() => reject(new Error('timeout')), timeout),
    ),
  ]);
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
    const isEmpty = await invoke<boolean>('is_dir_empty', {
      path: result,
    });
    if (!isEmpty) {
      await invoke('ensure_dir', {
        path: `${result}${sep()}${PROJECT_CONFIG.regName}`,
      });
      source.value = `${result}${sep()}${PROJECT_CONFIG.regName}`;
    } else source.value = result;
  } catch (e) {
    if (e instanceof Error) await error(e.toString());
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
