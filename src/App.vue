<template>
  <div class="main">
    <div v-show="!init" class="init-loading">
      <span class="fui-Spinner__spinner">
        <span class="fui-Spinner__spinnerTail"></span>
      </span>
    </div>
    <div v-show="init" class="content">
      <div class="image">
        <img src="./left.webp" :alt="PROJECT_CONFIG.title" />
      </div>
      <div class="right">
        <div class="title">{{ PROJECT_CONFIG.title }}</div>
        <div class="desc">{{ PROJECT_CONFIG.description }}</div>
        <div v-if="step === 1" class="actions">
          <div v-if="!isUpdate && !INSTALLER_CONFIG.is_uninstall" class="lnk">
            <Checkbox v-model="createLnk" />
            创建桌面快捷方式
          </div>
          <div v-if="!isUpdate && !INSTALLER_CONFIG.is_uninstall" class="read">
            <Checkbox v-model="acceptEula" />
            我已阅读并同意
            <a> 用户协议 </a>
          </div>
          <div v-if="INSTALLER_CONFIG.is_uninstall" class="read">
            <Checkbox v-model="deleteUserData" />
            同时删除用户数据
          </div>
          <div class="more">
            <span v-if="!isUpdate && !INSTALLER_CONFIG.is_uninstall">
              安装到
            </span>
            <span v-if="isUpdate && !INSTALLER_CONFIG.is_uninstall">
              更新到
            </span>
            <span v-if="INSTALLER_CONFIG.is_uninstall"> 卸载自 </span>
            <a @click="changeSource">{{ source }}</a>
          </div>
          <button
            v-if="!INSTALLER_CONFIG.is_uninstall"
            class="btn btn-install"
            @click="install"
            :disabled="!isUpdate && !acceptEula"
          >
            <IconSheild
              style="
                width: 20px;
                margin-right: 6px;
                margin-left: -6px;
                padding-top: 2px;
              "
              v-if="needElevate || INSTALLER_CONFIG.elevated"
            />
            {{ isUpdate ? '更新' : '安装' }}
          </button>
          <button
            v-if="INSTALLER_CONFIG.is_uninstall"
            class="btn btn-install"
            @click="uninstall"
          >
            <IconSheild
              style="
                width: 20px;
                margin-right: 6px;
                margin-left: -6px;
                padding-top: 2px;
              "
              v-if="needElevate || INSTALLER_CONFIG.elevated"
            />
            卸载
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
        <div class="finish" v-if="step === 4">
          <div class="finish-text">
            <CircleSuccess />
            您已安装最新版本
          </div>
          <button class="btn btn-install" @click="launch">启动</button>
        </div>
        <div class="uninstall" v-if="step === 5">
          <span class="fui-Spinner__spinner">
            <span class="fui-Spinner__spinnerTail"></span>
          </span>
          <button class="btn btn-install" disabled>卸载中</button>
        </div>
        <div class="finish" v-if="step === 6">
          <div class="finish-text">
            <CircleSuccess />
            卸载成功
          </div>
          <button class="btn btn-install" @click="exit">关闭</button>
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
  right: 8px;
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
.uninstall {
  height: 117px;
  display: flex;
  justify-content: center;
  align-items: center;
}

.uninstall .fui-Spinner__spinner {
  width: 40px;
  height: 40px;
  display: block;
  --fui-Spinner--strokeWidth: 4px;
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
import { onMounted, reactive, ref } from 'vue';
import { v4 as uuid } from 'uuid';
import { mapLimit } from 'async';
import Checkbox from './Checkbox.vue';
import CircleSuccess from './CircleSuccess.vue';
import { getCurrentWindow, invoke, listen, sep } from './tauri';
import { getDfsMetadata, runDfsDownload } from './dfs';
import {
  ipcCreateLnk,
  ipcCreateUninstaller,
  ipcFindProcessByName,
  ipcInstallRuntime,
  ipcKillProcess,
  ipcRmList,
  ipcRunUninstall,
  ipcWriteRegistry,
  ipPrepare,
  log,
  sendInsight,
} from './api/ipc';
import IconSheild from './IconSheild.vue';
import { version_compare } from './utils/version';
import { getRuntimeName } from './consts';

const init = ref(false);

const subStepList: ReadonlyArray<string> = [
  '获取最新版本',
  '校验更新内容',
  '下载和解压文件',
  '准备运行环境',
];

const isUpdate = ref<boolean>(false);
const acceptEula = ref<boolean>(true);
const createLnk = ref<boolean>(true);
const deleteUserData = ref<boolean>(false);
const step = ref<number>(1);
const subStep = ref<number>(0);
const needElevate = ref(true);

const current = ref<string>('');
const percent = ref<number>(0);
const source = ref<string>('');
const progressInterval = ref<number>(0);

const PROJECT_CONFIG: ProjectConfig = reactive({
  source: '',
  appName: 'Kachina',
  publisher: 'YuehaiTeam',
  regName: 'Kachina',
  exeName: 'inst.exe',
  uninstallName: 'uninst.exe',
  updaterName: 'update.exe',
  programFilesPath: 'Kachina',
  userDataPath: [],
  extraUninstallPath: [],
  title: 'Title',
  description: 'description',
  windowTitle: ' ',
  uacStrategy: 'prefer-admin',
});

const INSTALLER_CONFIG: InstallerConfig = reactive({
  install_path: '',
  install_path_exists: false,
  install_path_source: 'DEFAULT',
  is_uninstall: false,
  embedded_config: null,
  enbedded_metadata: null,
  embedded_files: [],
  embedded_index: [],
  exe_path: '',
  args: {
    target: null,
    uninstall: false,
    non_interactive: false,
    silent: false,
    online: false,
  },
  elevated: false,
});

const getInsightBase = () => {
  const qs = new URLSearchParams();
  if (INSTALLER_CONFIG.args.non_interactive) {
    qs.set('non_interactive', '1');
  }
  if (INSTALLER_CONFIG.args.silent) {
    qs.set('silent', '1');
  }
  if (INSTALLER_CONFIG.args.uninstall) {
    qs.set('uninstall', '1');
  }
  if (INSTALLER_CONFIG.args.online) {
    qs.set('online', '1');
  }
  if ((INSTALLER_CONFIG.embedded_index?.length || 0) > 0) {
    qs.set('pack', '1');
  }
  return `/${PROJECT_CONFIG.appName}?${qs.toString()}`;
};

async function getSource(): Promise<InstallerConfig> {
  return await invoke<InstallerConfig>('get_installer_config');
}

async function runInstall(): Promise<void> {
  step.value = 2;
  let latest_meta = INSTALLER_CONFIG.enbedded_metadata;
  let online_meta: InvokeGetDfsMetadataRes | null = null;
  try {
    online_meta = await getDfsMetadata(PROJECT_CONFIG.source);
  } catch (e) {
    log(e);
  }
  if (!latest_meta && !online_meta) {
    await error('获取更新信息失败，请检查网络连接');
    step.value = 1;
    return;
  } else if (!latest_meta) {
    latest_meta = online_meta;
    log('Local meta not found, use online meta');
  } else if (
    online_meta &&
    online_meta.tag_name !== latest_meta.tag_name &&
    version_compare(online_meta.tag_name, latest_meta.tag_name) > 0
  ) {
    log('Version update detected');
    if (
      !INSTALLER_CONFIG.args.non_interactive &&
      !INSTALLER_CONFIG.args.silent &&
      ((isUpdate.value &&
        (INSTALLER_CONFIG.embedded_index?.length || 0) <= 0) ||
        (await confirm('当前安装包不是最新版本，是否直接安装最新版本？')))
    ) {
      latest_meta = online_meta;
    } else {
      log('Has version update but use local meta');
    }
  } else {
    log('Local meta found, use local meta');
  }
  latest_meta = latest_meta as InvokeGetDfsMetadataRes;
  if (
    isUpdate.value &&
    latest_meta.installer &&
    !INSTALLER_CONFIG.enbedded_metadata
  ) {
    if (
      !latest_meta.hashed.find(
        (e) => e.file_name === PROJECT_CONFIG.updaterName,
      )
    ) {
      const installerMeta: DfsMetadataHashInfo = {
        file_name: PROJECT_CONFIG.updaterName,
        size: latest_meta.installer.size,
        md5: latest_meta.installer.md5,
        xxh: latest_meta.installer.xxh,
        installer: true,
      };
      latest_meta.hashed.push(installerMeta);
    }
  }
  await ipPrepare(needElevate.value);
  sendInsight(
    getInsightBase(),
    `${isUpdate.value ? 'update' : 'install'}/${INSTALLER_CONFIG.embedded_index?.length ? 'packed/' : ''}${latest_meta?.tag_name}`,
  );
  const target_exe_path = `${source.value}${sep()}${PROJECT_CONFIG.exeName}`;
  const runningExes =
    (await ipcFindProcessByName(PROJECT_CONFIG.exeName).catch(log)) || [];
  if (
    runningExes.find(
      (e) =>
        e[1].toLowerCase().replace(/\\/g, '/') ===
        target_exe_path.toLowerCase().replace(/\\/g, '/'),
    )
  ) {
    const result =
      INSTALLER_CONFIG.args.non_interactive ||
      INSTALLER_CONFIG.args.silent ||
      (await confirm(
        `检测到${PROJECT_CONFIG.appName}正在运行，是否结束进程并继续安装？`,
        '提示',
      ));
    if (!result) {
      step.value = 1;
      return;
    } else {
      try {
        await Promise.all(
          runningExes.map((e) => ipcKillProcess(e[0], needElevate.value)),
        );
      } catch (e) {
        await Promise.all(runningExes.map((e) => ipcKillProcess(e[0], true)));
      }
      return runInstall();
    }
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
      {
        id,
        source: source.value,
        hashAlgorithm: hashKey,
        fileList: latest_meta.hashed.map((e) => e.file_name),
      },
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
        strip_first_slash(e.file_name.toLowerCase()) ===
        strip_first_slash(item.file_name.toLowerCase()),
    );
    if (!local || local.hash !== item[hashKey as DfsMetadataHashType]) {
      let patch = latest_meta.patches?.find(
        (e) =>
          e.from[hashKey as DfsMetadataHashType] === local?.hash &&
          e.to[hashKey as DfsMetadataHashType] ===
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
        old_hash: local?.hash,
        unwritable: local?.unwritable || false,
      });
    }
  }
  if (diff_files.length === 0) {
    percent.value = 100;
    step.value = 4;
    await finishInstall(latest_meta);
    return;
  }
  // TODO: 检查占用需要用管理员权限运行
  // if (diff_files.find((e) => e.unwritable)) {
  //   if (
  //     !INSTALLER_CONFIG.args.non_interactive &&
  //     !INSTALLER_CONFIG.args.silent &&
  //     !(await confirm('检测到部分文件被占用，继续安装可能无法成功，是否继续？'))
  //   ) {
  //     step.value = 1;
  //     return;
  //   }
  // }
  console.log('Files to install:', diff_files);
  subStep.value = 2;
  current.value = '准备下载……';
  const total_size = diff_files.reduce((acc, cur) => acc + cur.size, 0);
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
    if (time_diff > 100) {
      stat.speed = (downloadedTotalSize - stat.speedLastSize) / time_diff;
      stat.speedLastSize = downloadedTotalSize;
      stat.lastTime = now;
    }
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
  }, 30);
  await mapLimit(diff_files, 6, async (item: (typeof diff_files)[0]) => {
    let hasError = false;
    for (let i = 0; i < 3; i++) {
      try {
        await runDfsDownload(
          PROJECT_CONFIG.source,
          INSTALLER_CONFIG.embedded_files || [],
          source.value,
          hashKey as DfsMetadataHashType,
          item,
          hasError,
          hasError || INSTALLER_CONFIG.args.online,
          needElevate.value,
        );
        break;
      } catch (e) {
        hasError = true;
        log(e);
        if (i === 2) {
          await error(`释放文件${item.file_name}失败: ${e}`, '出错了');
          throw e;
        }
      }
    }
  });
  clearInterval(progressInterval.value);
  if (
    latest_meta.deletes &&
    Array.isArray(latest_meta.deletes) &&
    latest_meta.deletes.length > 0
  ) {
    current.value = '删除旧版残留文件……';
    try {
      await ipcRmList(
        latest_meta.deletes.map((e) => `${source.value}${sep()}${e}`),
        needElevate.value,
      );
    } catch (e) {
      log(e);
    }
  }
  if (PROJECT_CONFIG.runtimes) {
    log('latest_meta.runtimes', PROJECT_CONFIG.runtimes);
    subStep.value = 3;
    current.value = '安装运行库……';
    for (const tag of PROJECT_CONFIG.runtimes) {
      log(`Installing runtime: ${tag}`);
      current.value = `安装${getRuntimeName(tag)}……`;
      try {
        await ipcInstallRuntime(
          tag,
          ({ payload }) => {
            const currentSize = formatSize(payload[0]);
            const targetSize = payload[1] ? formatSize(payload[1]) : '';
            if (payload[0] >= payload[1] - 1) {
              current.value = `安装 ${getRuntimeName(tag)} ……`;
            } else {
              current.value = `下载 ${getRuntimeName(tag)} ……<br>${currentSize}${targetSize ? ` / ${targetSize}` : ''}`;
            }
          },
          needElevate.value,
        );
      } catch (e) {
        log(e);
        await error(
          `安装${getRuntimeName(tag)}失败: ${e}，请手动安装`,
          '出错了',
        );
      }
    }
  }

  current.value = '很快就好……';
  await finishInstall(latest_meta);
  current.value = '安装完成';
  step.value = 3;
  percent.value = 100;
}

async function getLnkPath() {
  const [program, desktop] = await invoke<InvokeGetDirsRes>('get_dirs', {
    elevated: needElevate.value,
  });
  return {
    programFolder: `${program}${sep()}${PROJECT_CONFIG.appName}`,
    program: `${program}${sep()}${PROJECT_CONFIG.appName}${sep()}${PROJECT_CONFIG.appName}.lnk`,
    desktop: `${desktop}${sep()}${PROJECT_CONFIG.appName}.lnk`,
    uninstall: `${program}${sep()}${PROJECT_CONFIG.appName}${sep()}卸载${PROJECT_CONFIG.appName}.lnk`,
  };
}

async function finishInstall(
  latest_meta: InvokeGetDfsMetadataRes,
): Promise<void> {
  sendInsight(getInsightBase(), 'finish');
  const { program, desktop, uninstall } = await getLnkPath();
  const exePath = `${source.value}${sep()}${PROJECT_CONFIG.exeName}`;
  if (createLnk.value && !isUpdate.value) {
    await ipcCreateLnk(exePath, desktop, needElevate.value);
  }
  if (!isUpdate.value) {
    await ipcCreateLnk(exePath, program, needElevate.value).catch(log);
  }
  if (
    !isUpdate.value ||
    INSTALLER_CONFIG.install_path_source.startsWith('REG')
  ) {
    try {
      await ipcCreateUninstaller(
        source.value,
        PROJECT_CONFIG.uninstallName,
        PROJECT_CONFIG.updaterName,
        needElevate.value,
      );
    } catch (e) {
      error(`创建卸载程序失败: ${e}`, '出错了');
      log(e);
    }
    try {
      await ipcWriteRegistry(
        {
          reg_name: PROJECT_CONFIG.regName,
          name: PROJECT_CONFIG.appName,
          version: latest_meta.tag_name || '0.0',
          exe: `${source.value}${sep()}${PROJECT_CONFIG.exeName}`,
          source: source.value,
          uninstaller: `${source.value}${sep()}${PROJECT_CONFIG.uninstallName}`,
          metadata: JSON.stringify(latest_meta),
          size: latest_meta.hashed.reduce((acc, cur) => acc + cur.size, 0),
          publisher: PROJECT_CONFIG.publisher,
        },
        needElevate.value,
      );
    } catch (e) {
      error(`写入注册表失败: ${e}`, '出错了');
      log(e);
    }
    await ipcCreateLnk(
      `${source.value}${sep()}${PROJECT_CONFIG.uninstallName}`,
      uninstall,
      needElevate.value,
    ).catch(log);
  }
  if (INSTALLER_CONFIG.args.silent) {
    const win = getCurrentWindow();
    win.close();
  }
}

async function install(): Promise<void> {
  try {
    await runInstall();
  } catch (e) {
    log(e);
    const errstr =
      e instanceof Error
        ? e.stack || e.toString()
        : typeof e === 'string'
          ? e
          : JSON.stringify(e);
    await error(errstr);
    await sendInsight(getInsightBase(), 'error', { error: errstr });
    step.value = 1;
    subStep.value = 0;
    percent.value = 0;
    current.value = '';
    clearInterval(progressInterval.value);
    progressInterval.value = 0;
  }
}

onMounted(async () => {
  try {
    const win = getCurrentWindow();
    if (process.env.NODE_ENV === 'development') {
      await win.show();
    }
    const rsrc = await getSource();
    log('INSTALLER_CONFIG: ', rsrc);
    Object.assign(INSTALLER_CONFIG, rsrc);
    source.value =
      INSTALLER_CONFIG.args.target || INSTALLER_CONFIG.install_path;
    const seldir = await invoke<InvokeSelectDirRes>('select_dir', {
      exeName: PROJECT_CONFIG.exeName,
      silent: true,
      path: source.value,
    });
    if (seldir) {
      setUacByState(seldir.state, PROJECT_CONFIG.uacStrategy);
    }
    if (!rsrc.args.silent) {
      await win.show();
    }
    if (INSTALLER_CONFIG.embedded_config) {
      Object.assign(PROJECT_CONFIG, INSTALLER_CONFIG.embedded_config);
      if (process.env.NODE_ENV === 'development') {
        if (
          INSTALLER_CONFIG.embedded_files &&
          INSTALLER_CONFIG.embedded_files.length > 0 &&
          !INSTALLER_CONFIG.embedded_files.find((e) => e.name === '\0CONFIG')
        ) {
          error('打包错误，请确保配置文件被正确打包');
        }
      }
    } else if (process.env.NODE_ENV === 'development') {
      error('未找到配置文件，请将配置文件放在exe同目录下');
    } else {
      await error('安装包损坏，请重新下载');
      const win = getCurrentWindow();
      win.close();
      return;
    }
    if (INSTALLER_CONFIG.embedded_index && INSTALLER_CONFIG.embedded_files) {
      let hasWrongIndex = false;
      for (const i of INSTALLER_CONFIG.embedded_index) {
        const target = INSTALLER_CONFIG.embedded_files.find(
          (e) => e.name === i.name,
        );
        if (!target) {
          log('Unfound index', target, i);
          hasWrongIndex = true;
          continue;
        }
        if (target.offset !== i.offset || target.raw_offset !== i.raw_offset) {
          log('Wrong index: pack=', target, 'index=', i);
          hasWrongIndex = true;
        }
      }
      if (hasWrongIndex) {
        if (process.env.NODE_ENV === 'development') {
          error('打包错误，请确保索引文件正确');
        } else {
          await error('安装包损坏，请重新下载');
          const win = getCurrentWindow();
          win.close();
          return;
        }
      }
    }
    sendInsight(getInsightBase());
    if (INSTALLER_CONFIG.install_path_exists) isUpdate.value = true;
    await win.setTitle(PROJECT_CONFIG.windowTitle);
    INSTALLER_CONFIG.is_uninstall =
      INSTALLER_CONFIG.is_uninstall || INSTALLER_CONFIG.args.uninstall;
    if (INSTALLER_CONFIG.is_uninstall) {
      const uninstallConfig = await invoke(
        'read_uninstall_metadata',
        PROJECT_CONFIG,
      ).catch(log);
      log('UNINSTALL_METADATA: ', uninstallConfig);
      if (!uninstallConfig) {
        await error('未找到卸载配置文件，请重新安装后再卸载');
        if (process.env.NODE_ENV !== 'development') {
          const win = getCurrentWindow();
          win.close();
        }
        return;
      }
    }
    init.value = true;
    if (INSTALLER_CONFIG.args.silent || INSTALLER_CONFIG.args.non_interactive) {
      if (INSTALLER_CONFIG.args.uninstall || INSTALLER_CONFIG.is_uninstall) {
        uninstall();
      } else {
        install();
      }
    }
  } catch (e) {
    log(e);
    if (e instanceof Error)
      await error(e.stack || e.toString(), '安装程序初始化失败');
    else
      await error(
        typeof e === 'string' ? e : JSON.stringify(e),
        '安装程序初始化失败',
      );
    if (process.env.NODE_ENV !== 'development') {
      const win = getCurrentWindow();
      win.close();
    }
  }
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
  const mainExe = PROJECT_CONFIG.exeName;
  const fullPath = `${source.value}${sep()}${mainExe}`;
  await invoke('launch_and_exit', { path: fullPath });
}
async function exit() {
  const win = getCurrentWindow();
  win.close();
}

async function changeSource() {
  try {
    const seldir = await invoke<InvokeSelectDirRes>('select_dir', {
      path: source.value,
      exeName: PROJECT_CONFIG.exeName,
      silent: false,
    });
    if (seldir === null) return;
    log('SELECT_DIR: ', seldir);
    setUacByState(seldir.state, PROJECT_CONFIG.uacStrategy);
    isUpdate.value = seldir.upgrade;
    if (!seldir.empty && !seldir.upgrade) {
      const isDriveRoot = seldir.path.replace(/\\/g, '/').match(/^\w:\/$/);
      const confirmRes =
        isDriveRoot ||
        (await confirm(
          '您选择的目录不为空，是否创建新文件夹再安装？选【否】将可能影响原有数据。',
          '提示',
        ));
      if (confirmRes) {
        source.value =
          `${seldir.path}${sep()}${PROJECT_CONFIG.appName}`.replace(
            /\\\\/g,
            '\\',
          );
      } else {
        source.value = seldir.path;
      }
    } else {
      source.value = seldir.path;
    }
  } catch (e) {
    if (e instanceof Error) await error(e.stack || e.toString());
    else await error(JSON.stringify(e));
    throw e;
  }
}

async function error(message: string, title = '出错了'): Promise<void> {
  await invoke('error_dialog', {
    message: message.replace(new RegExp(location.origin, 'g'), ''),
    title,
  });
  if (INSTALLER_CONFIG.args.silent) {
    const win = getCurrentWindow();
    win.close();
  }
}
async function confirm(message: string, title = '提示'): Promise<boolean> {
  return await invoke<boolean>('confirm_dialog', { message, title });
}
async function uninstall() {
  step.value = 5;
  sendInsight(getInsightBase(), 'uninstall');
  try {
    const uninstallConfig = (await invoke(
      'read_uninstall_metadata',
      PROJECT_CONFIG,
    )) as InvokeGetDfsMetadataRes;
    if (!uninstallConfig) {
      throw new Error('未找到卸载配置文件，请重新安装后再卸载');
    }
    await ipPrepare(needElevate.value);
    const { programFolder, desktop } = await getLnkPath();
    await ipcRunUninstall(
      {
        source: INSTALLER_CONFIG.install_path,
        files: [
          ...uninstallConfig.hashed.map((e) => e.file_name),
          PROJECT_CONFIG.updaterName,
        ],
        user_data_path: deleteUserData.value
          ? PROJECT_CONFIG.userDataPath.map(replacePathEnvirables)
          : [],
        extra_uninstall_path: [
          ...(PROJECT_CONFIG.extraUninstallPath?.map(replacePathEnvirables) ||
            []),
          programFolder,
          desktop,
        ],
        reg_name: PROJECT_CONFIG.regName,
        uninstall_name: PROJECT_CONFIG.uninstallName,
      },
      needElevate.value,
    );
    step.value = 6;
    if (INSTALLER_CONFIG.args.silent) {
      const win = getCurrentWindow();
      win.close();
    }
  } catch (e) {
    log(e);
    const errstr =
      e instanceof Error
        ? e.stack || e.toString()
        : typeof e === 'string'
          ? e
          : JSON.stringify(e);
    await error(errstr);
    await sendInsight(getInsightBase(), 'error', { error: errstr });
    step.value = 1;
  }
}

function tplReplace(template: string, data: Record<string, string>): string {
  const regex = /\${(.*?)}/g;
  return template.replace(regex, (_match, key) => {
    return typeof data[key] !== 'undefined' ? data[key] : '';
  });
}
function replacePathEnvirables(path: string): string {
  return tplReplace(path, {
    INSTALL_PATH: INSTALLER_CONFIG.install_path,
    APP_NAME: PROJECT_CONFIG.appName,
  });
}
function setUacByState(
  state: 'Unwritable' | 'Writable' | 'Private',
  uacStrategy: ProjectConfig['uacStrategy'],
) {
  needElevate.value = false;
  switch (uacStrategy) {
    case 'force':
      needElevate.value = true;
      break;
    case 'prefer-admin':
      needElevate.value = state !== 'Private';
      break;
    case 'prefer-user':
      needElevate.value = state === 'Unwritable';
      break;
  }
}
window.ipcInstallRuntime = ipcInstallRuntime;
</script>
