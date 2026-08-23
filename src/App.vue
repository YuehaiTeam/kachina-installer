<template>
  <div class="main">
    <div v-show="init === 0" class="init-loading">
      <span class="fui-Spinner__spinner">
        <span class="fui-Spinner__spinnerTail"></span>
      </span>
    </div>
    <div
      v-show="init === 2 && !dialog"
      class="content"
      :class="{ borderless: PROJECT_CONFIG.windowBorderless }"
    >
      <div class="controls" v-if="PROJECT_CONFIG.windowBorderless">
        <button class="cont-minimize" @click="minimize">
          <IconMinimize />
        </button>
        <button class="cont-close" @click="close">
          <IconClose />
        </button>
      </div>
      <div class="image">
        <img
          v-if="!useDynamicCss"
          :src="imageSource"
          :alt="PROJECT_CONFIG.title"
        />
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
            <span>
              <template
                v-if="
                  !INSTALLER_CONFIG.is_uninstall &&
                  Array.isArray(PROJECT_CONFIG.source) &&
                  PROJECT_CONFIG.source.length > 1 &&
                  !INSTALLER_CONFIG.embedded_index?.length
                "
              >
                <span>从 </span>
                <a @click="dialog = 'source'" title="点击切换安装源">
                  {{
                    PROJECT_CONFIG.source.find((e) => e.uri === selectedSource)
                      ?.name
                  }}<template v-if="installMode === 'mirrorc'"
                    >({{ mirrorcKey ? markedKey : '无CDK' }})</template
                  >
                  <IconEdit />
                </a>
              </template>
              <span v-if="!isUpdate && !INSTALLER_CONFIG.is_uninstall">
                安装到
              </span>
              <span v-if="isUpdate && !INSTALLER_CONFIG.is_uninstall">
                更新到
              </span>
              <span v-if="INSTALLER_CONFIG.is_uninstall"> 卸载自 </span>
            </span>
            <a
              v-if="!INSTALLER_CONFIG.is_uninstall"
              @click="changeSource"
              title="点击修改安装路径"
              >{{ source }}<IconEdit
            /></a>
            <a v-else>{{ source }}</a>
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
              v-for="(i, a) in installMode === 'mirrorc'
                ? subStepListMirrorc
                : subStepList"
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
          <button class="btn btn-install" disabled>
            <span
              class="fui-Spinner__spinner"
              style="width: 16px; height: 16px; margin-right: 8px"
            >
              <span class="fui-Spinner__spinnerTail"></span>
            </span>
            卸载中
          </button>
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
    <SourceDialog
      v-show="dialog === 'source'"
      v-if="Array.isArray(PROJECT_CONFIG.source)"
      :title="PROJECT_CONFIG.title"
      :sources="PROJECT_CONFIG.source"
      :selected-uri="selectedSource"
      :forced-id="INSTALLER_CONFIG.args.source"
      :active="dialog === 'source'"
      @select="changeSelectedSource"
    />
    <MirrorcDialog
      v-show="dialog === 'mirrorc'"
      :app-name="PROJECT_CONFIG.appName"
      :source-url="mirrorcTempUrl"
      :initial-key="mirrorcKey"
      @cancel="dialog = ''"
      @applied="onMirrorcApplied"
    />
    <component :is="'style'" v-if="useDynamicCss">{{ dynamicCss }}</component>
  </div>
</template>

<style scoped>
.main {
  min-height: 100vh;
  app-region: drag;
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
  .borderless & {
    padding-top: 44px;
  }
}

.title {
  font-size: 25px;
  padding: 2px 10px 6px;
}

.btn-install {
  app-region: no-drag;
  height: 40px;
  width: 140px;
  position: absolute;
  bottom: 20px;
  right: 8px;
  &.btn-install-2rd {
    right: 158px;
    width: 100px;
  }
}

.actions {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding-top: 16px;
  app-region: no-drag;
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
  svg {
    width: 12px;
    position: relative;
    top: 2px;
    padding-left: 2px;
    opacity: 0.8;
  }

  span {
    span {
      opacity: 0.8;
    }
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
.controls {
  app-region: no-drag;
  position: absolute;
  right: 0;
  top: 0;
  z-index: 9999;
  height: 32px;
  display: flex;
  & > button {
    width: 45px;
    height: 32px;
    overflow: hidden;
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    svg {
      height: 14px;
    }
    appearance: none;
    background: transparent;
    border: 0;
    color: inherit;
    &:active {
      opacity: 0.8;
    }
  }
  .cont-close:hover {
    background: #c42b1c;
  }

  .cont-minimize:hover {
    background: rgba(255, 255, 255, 0.07);
  }
}
</style>
<script lang="ts" setup>
import { computed, onMounted, reactive, ref, watch } from 'vue';
import Checkbox from './Checkbox.vue';
import CircleSuccess from './CircleSuccess.vue';
import IconEdit from './IconEdit.vue';
import { getCurrentWindow, invoke, sep } from './tauri';
import { error, log, sendInsight } from './api/ipc';
import IconSheild from './IconSheild.vue';
import SourceDialog from './components/SourceDialog.vue';
import MirrorcDialog from './components/MirrorcDialog.vue';
import { bootstrap } from './bootstrap';
import { runSession } from './session';
import {
  confirmDialog,
  dialogError,
  insightBase,
  stringifyError,
  stringifyErrorLog,
  uacNeeded,
} from './ui';
import { InstallerConfig, InvokeSelectDirRes, ProjectConfig } from './types.ts';
import IconMinimize from './IconMinimize.vue';
import IconClose from './IconClose.vue';

const init = ref(0);

const subStepList: ReadonlyArray<string> = [
  '获取最新版本',
  '校验更新内容',
  '下载和解压文件',
  '准备运行环境',
];
const subStepListMirrorc: ReadonlyArray<string> = [
  '从 Mirror酱 获取最新版本',
  '下载数据包',
  '解压文件',
  '准备运行环境',
];

const isUpdate = ref(false);
const acceptEula = ref(true);
const createLnk = ref(true);
const deleteUserData = ref(false);
const step = ref(1);
const subStep = ref(0);
const needElevate = ref(true);
const current = ref('');
const percent = ref(0);
const source = ref('');
const dialog = ref<'' | 'mirrorc' | 'source'>('');
const imageSource = ref('');
const dynamicCss = ref('');
const useDynamicCss = ref(false);
const selectedSource = ref('');
const mirrorcKey = ref('');
const mirrorcTempUrl = ref('');

const installMode = computed<'default' | 'mirrorc'>(() =>
  selectedSource.value.startsWith('mirrorc://') ? 'mirrorc' : 'default',
);
const markedKey = computed(
  () =>
    mirrorcKey.value.substring(0, 4) +
    '****' +
    mirrorcKey.value.substring(mirrorcKey.value.length - 4),
);

watch(
  () => installMode.value,
  async (mode) => {
    if (mode === 'mirrorc' && !mirrorcKey.value) {
      try {
        mirrorcKey.value = await invoke('wincred_read', {
          target: `KachinaInstaller_MirrorChyanCDK_${PROJECT_CONFIG.appName}`,
        });
      } catch {}
    }
  },
);

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
  ignoreFolderPath: [],
  extraUninstallPath: [],
  title: 'Title',
  description: 'description',
  windowTitle: ' ',
  uacStrategy: 'prefer-admin',
  windowBorderless: false,
});

const INSTALLER_CONFIG: InstallerConfig = reactive({
  install_path: '',
  install_path_exists: false,
  install_path_source: 'DEFAULT',
  is_uninstall: false,
  embedded_config: null,
  enbedded_metadata: null,
  embedded_image: null,
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

function sessionInput() {
  return {
    install_path: source.value,
    source_uri: selectedSource.value,
    create_lnk: createLnk.value,
    delete_user_data: deleteUserData.value,
    mirrorc_cdk: mirrorcKey.value || null,
  };
}

function resetProgress() {
  step.value = 1;
  subStep.value = 0;
  percent.value = 0;
  current.value = '';
}

async function startSession(kind: 'install' | 'uninstall') {
  return runSession(
    kind,
    sessionInput(),
    insightBase(INSTALLER_CONFIG, PROJECT_CONFIG),
    () => {
      dialog.value = 'source';
    },
    (event) => {
      subStep.value = event.sub_step;
      percent.value = event.percent;
      current.value = event.current;
    },
  );
}

async function install(): Promise<void> {
  if (installMode.value === 'mirrorc' && !mirrorcKey.value) {
    changeSelectedSource(selectedSource.value);
    return;
  }
  step.value = 2;
  try {
    const result = await startSession('install');
    if (result.cancelled) {
      resetProgress();
      return;
    }
    percent.value = 100;
    step.value = result.already_latest ? 4 : 3;
  } catch (e) {
    error(e);
    sendInsight(insightBase(INSTALLER_CONFIG, PROJECT_CONFIG), 'error', {
      error: stringifyErrorLog(e),
    });
    await dialogError(
      stringifyError(e),
      '出错了',
      INSTALLER_CONFIG.args.silent,
    );
    resetProgress();
  }
}

async function uninstall() {
  step.value = 5;
  sendInsight(insightBase(INSTALLER_CONFIG, PROJECT_CONFIG), 'uninstall');
  try {
    await startSession('uninstall');
    step.value = 6;
  } catch (e) {
    error(e);
    const errstr = stringifyErrorLog(e);
    await dialogError(errstr, '出错了', INSTALLER_CONFIG.args.silent);
    await sendInsight(insightBase(INSTALLER_CONFIG, PROJECT_CONFIG), 'error', {
      error: errstr,
    });
    step.value = 1;
  }
}

onMounted(async () => {
  try {
    const result = await bootstrap(PROJECT_CONFIG, INSTALLER_CONFIG);
    if (!result) {
      return;
    }
    selectedSource.value = result.selectedSource;
    source.value = result.source;
    isUpdate.value = result.isUpdate;
    needElevate.value = result.needElevate;
    createLnk.value = result.createLnk;
    deleteUserData.value = result.deleteUserData;
    if (result.mirrorcKey) {
      mirrorcKey.value = result.mirrorcKey;
    }
    imageSource.value = result.theme.imageSource;
    dynamicCss.value = result.theme.dynamicCss;
    useDynamicCss.value = result.theme.useDynamicCss;
    init.value = 2;
    if (result.autoRun === 'uninstall') {
      uninstall();
    } else if (result.autoRun === 'install') {
      install();
    }
  } catch (e) {
    error(e);
    await dialogError(
      stringifyErrorLog(e),
      '安装程序初始化失败',
      INSTALLER_CONFIG.args.silent,
    );
    if (process.env.NODE_ENV !== 'development') {
      getCurrentWindow().close();
    }
  }
});

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
    needElevate.value = uacNeeded(seldir.state, PROJECT_CONFIG.uacStrategy);
    isUpdate.value = seldir.upgrade;
    if (!seldir.empty && !seldir.upgrade) {
      const isDriveRoot = seldir.path.replace(/\\/g, '/').match(/^\w:\/$/);
      const confirmRes =
        isDriveRoot ||
        (await confirmDialog(
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
    await dialogError(
      stringifyErrorLog(e),
      '出错了',
      INSTALLER_CONFIG.args.silent,
    );
    throw e;
  }
}

async function changeSelectedSource(url: string) {
  const isMirrorc = url.startsWith('mirrorc://');
  dialog.value = isMirrorc ? 'mirrorc' : '';
  if (isMirrorc) {
    try {
      mirrorcKey.value = await invoke('wincred_read', {
        target: `KachinaInstaller_MirrorChyanCDK_${PROJECT_CONFIG.appName}`,
      });
    } catch (e) {
      console.warn(e);
    }
    mirrorcTempUrl.value = url;
  } else {
    selectedSource.value = url;
    mirrorcTempUrl.value = '';
  }
}

function onMirrorcApplied(payload: { url: string; key: string }) {
  mirrorcKey.value = payload.key;
  selectedSource.value = payload.url;
  dialog.value = '';
}

const minimize = async () => {
  const win = getCurrentWindow();
  win.minimize();
};
const close = async () => {
  const win = getCurrentWindow();
  win.close();
};
</script>
