import { invoke, listen } from './tauri';
import { v4 as uuid } from 'uuid';

const connectableOrigins = new Set();

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
  ]) as Promise<Response>;
}

export const getDfsUrl = async (key: string): Promise<string> => {
  const dfs_result = await invoke<InvokeGetDfsRes>('get_dfs', {
    path: `bgi/hashed/${key}`,
  });
  let url = dfs_result.url;
  if (!url && dfs_result.tests && dfs_result.tests.length > 0) {
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
  return url;
};

export const runDfsDownload = async (
  local: Embedded[],
  source: string,
  hashKey: DfsMetadataHashType,
  item: DfsUpdateTask,
  disable_patch = false,
  disable_local = false,
) => {
  const id = uuid();
  let filename_with_first_slash = item.file_name.startsWith('/')
    ? item.file_name
    : `/${item.file_name}`;
  let idUnListen = await listen<number>(id, ({ payload }) => {
    item.downloaded = payload;
  });
  item.running = true;
  try {
    const hasLocalFile = local.find((l) => l.name === item[hashKey]);
    if (hasLocalFile && !disable_local) {
      await invoke('local_decompress', {
        id,
        ...hasLocalFile,
        target: source + filename_with_first_slash,
      });
      console.log('>LOCAL', filename_with_first_slash);
    } else if (item.lpatch && !disable_patch && !disable_local) {
      const hash = `${item.lpatch.from[hashKey]}_${item.lpatch.to[hashKey]}`;
      const url = await getDfsUrl(hash);
      console.log('>LPATCH', filename_with_first_slash, item.lpatch, url);
      await invoke('local_decompress_and_online_hpatch', {
        id,
        url,
        name: item.patch?.from[hashKey] as string,
        target: source + filename_with_first_slash,
        diffSize: item.patch?.size as number,
      });
    } else if (item.patch && !disable_patch) {
      const hash = `${item.patch.from[hashKey]}_${item.patch.to[hashKey]}`;
      const url = await getDfsUrl(hash);
      console.log('>PATCH', filename_with_first_slash, item.patch, url);
      await invoke('download_and_decompress_and_hpatch', {
        id,
        url,
        target: source + filename_with_first_slash,
        diffSize: item.patch.size,
      });
    } else {
      const hash = item[hashKey] as string;
      const url = await getDfsUrl(hash);
      console.log('>DOWNLOAD', filename_with_first_slash, url);
      await invoke('download_and_decompress', {
        id,
        url,
        target: source + filename_with_first_slash,
      });
    }
  } catch (e) {
    console.error(e);
    item.downloaded = 0;
    throw e;
  } finally {
    idUnListen();
    item.running = false;
  }
  item.downloaded = item.patch ? item.patch.size : item.size;
};
