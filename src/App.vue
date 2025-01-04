<template>
  <div class="content">
    <div class="image">
      <img src="./left.png" alt="BetterGI" />
    </div>
    <div class="right">
      <div class="title">BetterGI</div>
      <div class="desc">更好的原神，免费且开源</div>
      <div v-if="step === 1" class="actions">
        <div v-if="!isUpdate" class="lnk">
          <div class="checkbox">
            <input type="checkbox" class="checkbox-inn" />
            <div class="checkbox-ind"></div>
          </div>
          创建桌面快捷方式
        </div>
        <div v-if="!isUpdate" class="read">
          <div class="checkbox">
            <input type="checkbox" class="checkbox-inn" />
            <div class="checkbox-ind"></div>
          </div>
          我已阅读并同意
          <a> 用户协议 </a>
        </div>
        <div class="more">
          <span v-if="!isUpdate">安装到</span>
          <span v-if="isUpdate">更新到</span>
          <a @click="changeSource">{{ source }}</a>
        </div>
        <button class="btn btn-install" @click="install">
          {{ isUpdate ? '更新' : '安装' }}
        </button>
      </div>
      <div class="progress" v-if="step === 2">
        <div class="step-desc">
          <div
            v-for="(i, a) in substeps"
            class="substep"
            :class="{ done: a < substep }"
            v-show="a <= substep"
            :key="i"
          >
            <span v-if="a === substep" class="fui-Spinner__spinner">
              <span class="fui-Spinner__spinnerTail"></span>
            </span>
            <span v-else class="substep-done">
              <svg
                fill="currentColor"
                viewBox="0 0 20 20"
                xmlns="http://www.w3.org/2000/svg"
              >
                <path
                  d="M10 2a8 8 0 1 1 0 16 8 8 0 0 1 0-16Zm0 1a7 7 0 1 0 0 14 7 7 0 0 0 0-14Zm3.36 4.65c.17.17.2.44.06.63l-.06.07-4 4a.5.5 0 0 1-.64.07l-.07-.06-2-2a.5.5 0 0 1 .63-.77l.07.06L9 11.3l3.65-3.65c.2-.2.51-.2.7 0Z"
                  fill="currentColor"
                ></path>
              </svg>
            </span>
            <div>{{ i }}</div>
          </div>
        </div>
        <div class="current-status" v-html="current"></div>
        <div class="progress-bar" :style="{ width: `${percent}%` }"></div>
      </div>
      <div class="finish" v-if="step === 3">
        <div class="finish-text">
          <svg
            fill="currentColor"
            viewBox="0 0 20 20"
            xmlns="http://www.w3.org/2000/svg"
          >
            <path
              d="M10 2a8 8 0 1 1 0 16 8 8 0 0 1 0-16Zm0 1a7 7 0 1 0 0 14 7 7 0 0 0 0-14Zm3.36 4.65c.17.17.2.44.06.63l-.06.07-4 4a.5.5 0 0 1-.64.07l-.07-.06-2-2a.5.5 0 0 1 .63-.77l.07.06L9 11.3l3.65-3.65c.2-.2.51-.2.7 0Z"
              fill="currentColor"
            ></path>
          </svg>
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
    color: var(--colorBrandForegroundLink);
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
    color: var(--colorBrandForegroundLink);
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
import { ref, onMounted } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { message, open } from '@tauri-apps/plugin-dialog';
import { mkdir, readDir } from '@tauri-apps/plugin-fs';
import { v4 as uuid } from 'uuid';
import { mapLimit } from 'async';
import { sep } from '@tauri-apps/api/path';
const isUpdate = ref(false);
const step = ref(1);
const substep = ref(0);
const substeps = ['获取最新版本信息', '检查更新内容', '下载并安装'];
const current = ref('');
const percent = ref(0);
const source = ref('');
const progressInterval = ref(0);

const PROJECT_CONFIG = {
  exeName: 'BetterGI.exe',
  regName: 'BetterGI',
  programFilesPath: 'BetterGI\\BetterGI',
  dfs_path: 'bgi',
  title: 'BetterGI',
  description: '更好的原神，免费且开源',
  windowTitle: 'BetterGI 安装程序',
};

const getSource = async () => {
  const result = (await invoke('get_install_source', PROJECT_CONFIG)) as [
    string,
    boolean,
  ];
  return result;
};
const runinstall = async () => {
  step.value = 2;
  const latest_meta = (await invoke('get_dfs_metadata', {
    prefix: `${PROJECT_CONFIG.dfs_path}`,
  })) as {
    tag_name: string;
    hashed: { file_name: string; md5: string; size: number }[];
  };
  substep.value = 1;
  percent.value = 5;
  let id = uuid();
  let unlisten = await listen(id, ({ payload }) => {
    const [currentValue, total] = payload as number[];
    current.value = `${currentValue} / ${total}`;
    percent.value = 5 + (currentValue / total) * 15;
  });
  const local_meta = (
    (await invoke('deep_readdir_with_metadata', {
      id,
      source: source.value,
    })) as { file_name: string; md5: string; size: number }[]
  ).map((e) => {
    return {
      ...e,
      file_name: e.file_name.replace(source.value, ''),
    };
  });
  unlisten();
  current.value = '校验本地文件……';
  const diff_files = [] as {
    file_name: string;
    md5: string;
    size: number;
  }[];
  const strip_first_slash = (s: string) => {
    let ss = s.replace(/\\/g, '/');
    if (ss.startsWith('/')) {
      return ss.slice(1);
    }
    return ss;
  };
  for (const item of latest_meta.hashed) {
    const local = local_meta.find(
      (e) =>
        strip_first_slash(e.file_name) === strip_first_slash(item.file_name),
    );
    if (!local || local.md5 !== item.md5) {
      diff_files.push(item);
    }
  }
  if (diff_files.length === 0) {
    percent.value = 100;
    step.value = 3;
    return;
  }
  substep.value = 2;
  current.value = '准备下载……';
  const total_size = diff_files.reduce((acc, cur) => acc + cur.size, 0);
  let stat = {
    downloaded_total_size: 0,
    speed_last_size: 0,
    last_time: performance.now(),
    speed: 0,
    runningTasks: {} as Record<string, string>,
  };
  progressInterval.value = setInterval(() => {
    const now = performance.now();
    const time_diff = now - stat.last_time;
    stat.speed =
      (stat.downloaded_total_size - stat.speed_last_size) / time_diff;
    stat.speed_last_size = stat.downloaded_total_size;
    stat.last_time = now;
    const speed = formatSize(stat.speed * 1000);
    const downloaded = formatSize(stat.downloaded_total_size);
    const total = formatSize(total_size);
    current.value =
      `${downloaded} / ${total} (${speed}/s)<div class="d-single-list"><div class="d-single">` +
      Object.values(stat.runningTasks).join('</div><div class="d-single">') +
      '</div></div>';
    percent.value = 20 + (stat.downloaded_total_size / total_size) * 80;
  }, 400);
  await mapLimit(diff_files, 5, async (item: (typeof diff_files)[0]) => {
    stat.runningTasks[item.file_name] =
      `<span class="d-single-filename">${basename(item.file_name)}</span><span class="d-single-progress">0%</span>`;
    const dfs_result = (await invoke('get_dfs', {
      path: `bgi/hashed/${item.md5}`,
    })) as { url?: string; tests?: [string, string][]; source: string };
    const id = uuid();
    let last_downloaded_size = 0;
    let unlisten = await listen(id, ({ payload }) => {
      let current_size = payload as number;
      const size_diff = current_size - last_downloaded_size;
      last_downloaded_size = current_size;
      stat.downloaded_total_size += size_diff;
      stat.runningTasks[item.file_name] =
        `<span class="d-single-filename">${basename(item.file_name)}</span><span class="d-single-progress">${Math.round(current_size / item.size)}%</span>`;
    });
    let filename_with_first_slash = item.file_name.startsWith('/')
      ? item.file_name
      : `/${item.file_name}`;
    console.log('dfs result is', dfs_result);
    let url = dfs_result.url;
    if (!url && (dfs_result.tests?.length || 0) > 0) {
      const tests = dfs_result.tests as [string, string][];
      if (tests.length > 0) {
        const now = performance.now();
        const result = await Promise.race(
          tests.map((test) => {
            return fetchWithTimeout(test[0], { method: 'HEAD' })
              .then((response) => {
                if (response.ok) {
                  return { url: test[1], time: performance.now() - now };
                }
                throw new Error('not ok');
              })
              .catch(() => {
                return { url: test[0], time: -1 };
              });
          }),
        );
        if (result.time > 0) {
          url = result.url;
        }
      }
    }
    if (!url && dfs_result.source) {
      url = dfs_result.source;
    }
    if (!url) {
      throw new Error('没有可用的下载节点：' + JSON.stringify(dfs_result));
    }
    const run_dl = async () => {
      try {
        await invoke('download_and_decompress', {
          id,
          url: url,
          target: source.value + filename_with_first_slash,
        });
      } catch (e) {
        console.error(e);
        stat.downloaded_total_size -= last_downloaded_size;
        throw e;
      } finally {
        unlisten();
        delete stat.runningTasks[item.file_name];
      }
      const size_diff = item.size - last_downloaded_size;
      stat.downloaded_total_size += size_diff;
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
  current.value = '安装完成';
  step.value = 3;
  percent.value = 100;
};
const install = async () => {
  try {
    await runinstall();
  } catch (e) {
    message((e as Error).toString(), {
      title: '出错了',
      kind: 'error',
    });
    step.value = 1;
    substep.value = 0;
    percent.value = 0;
    current.value = '';
    clearInterval(progressInterval.value);
    progressInterval.value = 0;
  }
};
onMounted(async () => {
  const [sourcePath, sourceExists] = await getSource();
  source.value = sourcePath;
  if (sourceExists) {
    isUpdate.value = true;
  }
  const win = getCurrentWindow();
  win.setTitle(PROJECT_CONFIG.windowTitle);
  win.show();
});
const formatSize = (size: number) => {
  if (size < 1024) {
    return `${size.toFixed(2)} B`;
  }
  if (size < 1024 * 1024) {
    return `${(size / 1024).toFixed(2)} KB`;
  }
  return `${(size / 1024 / 1024).toFixed(2)} MB`;
};
const basename = (path: string) => {
  return path.split('/').pop() as string;
};
const fetchWithTimeout = (
  url: string,
  options: RequestInit,
  timeout = 2000,
): Promise<Response> => {
  return Promise.race([
    fetch(url, options),
    new Promise((_, reject) =>
      setTimeout(() => reject(new Error('timeout')), timeout),
    ),
  ]) as Promise<Response>;
};
const launch = async () => {
  const mainExe = `BetterGI.exe`;
  const fullPath = `${source.value}/${mainExe}`;
  await invoke('launch_and_exit', { path: fullPath });
};
const changeSource = async () => {
  const result = await open({
    defaultPath: source.value,
    directory: true,
    canCreateDirectories: true,
    multiple: false,
  });
  if(result === null) return;
  const dirInfo = await readDir(result);
  if(dirInfo.length!==0) {
    await mkdir(`${result}${sep()}BetterGI`);
    source.value = `${result}${sep()}BetterGI`;
  } else {
    source.value = result;
  }
};
</script>
