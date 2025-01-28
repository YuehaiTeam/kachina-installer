import { hybridPatch, InstallFile } from './api/installFile';
import { ipc, log } from './api/ipc';
import { invoke } from './tauri';

const connectableOrigins = new Set();

export const dfsIndexCache = new Map<
  string,
  {
    index: Map<string, Embedded>;
    metadata: InvokeGetDfsMetadataRes;
    installer_end: number;
  }
>();

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

export const dfsSourceReg =
  /^(?:(dfs)\+)?(?:(hashed|packed|auto)\+)?(http(?:s)?:\/\/(?:.*?))$/;

export const getDfsSourceType = (
  source: string,
): {
  remote: 'dfs' | 'direct';
  storage: 'hashed' | 'packed';
  url: string;
} => {
  const match = source.match(dfsSourceReg);
  if (!match) throw new Error('Invalid dfs source: ' + source);
  const remote = ['dfs', 'direct'].includes(match[1]) ? match[1] : 'direct';
  let storage = ['hashed', 'packed', 'auto'].includes(match[2])
    ? match[2]
    : 'auto';
  if (storage === 'auto') {
    const url = new URL(match[3]);
    if (url.pathname.endsWith('.exe')) {
      storage = 'packed';
    } else if (url.pathname.endsWith('.json')) {
      storage = 'hashed';
    }
  }
  if (storage === 'auto') {
    throw new Error('Invalid dfs source: ' + source);
  }
  return {
    remote: remote as 'dfs' | 'direct',
    storage: storage as 'hashed' | 'packed',
    url: match[3],
  };
};
export const getDfsMetadata = async (
  source: string,
): Promise<InvokeGetDfsMetadataRes> => {
  const { remote, storage, url } = getDfsSourceType(source);
  const full_file_url = remote === 'direct' ? url : await getDfsFileUrl(url);
  if (storage === 'hashed') {
  } else {
    if (dfsIndexCache.has(source)) {
      return dfsIndexCache.get(source)?.metadata as InvokeGetDfsMetadataRes;
    } else {
      await refreshDfsIndex(source, full_file_url);
      if (dfsIndexCache.has(source)) {
        return dfsIndexCache.get(source)?.metadata as InvokeGetDfsMetadataRes;
      }
    }
  }
  throw new Error('Get metadata failed');
};
export async function refreshDfsIndex(source: string, binurl: string) {
  const pre_index: [number, number[]] = await invoke('get_http_with_range', {
    url: binurl,
    offset: 0,
    size: 256,
  });
  let bufStr = '';
  for (let i = 0; i < pre_index[1].length; i++) {
    bufStr += String.fromCharCode(pre_index[1][i]);
  }
  const header_offset = bufStr.indexOf('!KachinaInstaller!');
  if (header_offset === -1) {
    throw new Error('Invalid remote index');
  }
  const index_offset = header_offset + 18;
  const dataView = new DataView(new Uint8Array(pre_index[1]).buffer);
  const index_start = dataView.getUint32(index_offset, false);
  const config_sz = dataView.getUint32(index_offset + 4, false);
  const theme_sz = dataView.getUint32(index_offset + 8, false);
  const index_sz = dataView.getUint32(index_offset + 12, false);
  const metadata_sz = dataView.getUint32(index_offset + 16, false);
  const data_end = index_start + index_sz + config_sz + theme_sz + metadata_sz;
  const index: [number, number][] = await invoke('get_http_with_range', {
    url: binurl,
    offset: index_start,
    size: data_end - index_start,
  });
  const index_data = new Uint8Array(index[1]);
  const index_view = new DataView(index_data.buffer);
  const segments: {
    config?: ProjectConfig;
    metadata?: InvokeGetDfsMetadataRes;
    theme?: string;
    index?: Map<string, Embedded>;
  } = {};
  let offset = 0;
  while (offset < index_data.length) {
    const str =
      String.fromCharCode(index_data[offset]) +
      String.fromCharCode(index_data[offset + 1]) +
      String.fromCharCode(index_data[offset + 2]) +
      String.fromCharCode(index_data[offset + 3]);
    if (str !== '!in\0'.toUpperCase()) {
      offset++;
      continue;
    }
    log('found segment', offset);
    try {
      // 4byte magic, 2byte name_len, dyn name, 4byte size, dyn data
      offset += 4;
      const name_len = index_view.getUint16(offset, false);
      log('name_len', name_len);
      offset += 2;
      const name = new TextDecoder().decode(
        index_data.slice(offset, offset + name_len),
      );
      log('name', name);
      offset += name_len;
      const size = index_view.getUint32(offset, false);
      offset += 4;
      const data = index_data.slice(offset, offset + size);
      offset += size;
      switch (name) {
        case '\0CONFIG':
          segments.config = JSON.parse(new TextDecoder().decode(data));
          break;
        case '\0META':
          segments.metadata = JSON.parse(new TextDecoder().decode(data));
          break;
        case '\0THEME':
          segments.theme = new TextDecoder().decode(data);
          break;
        case '\0INDEX':
          const index_view = new DataView(data.buffer);
          const index = new Map<string, Embedded>();
          let idx_offset = 0;
          while (idx_offset < data.length) {
            const name_len = data[idx_offset];
            idx_offset++;
            const name = new TextDecoder().decode(
              data.slice(idx_offset, idx_offset + name_len),
            );
            idx_offset += name_len;
            const size = index_view.getUint32(idx_offset, false);
            idx_offset += 4;
            const offset = index_view.getUint32(idx_offset, false);
            idx_offset += 4;
            index.set(name, {
              name,
              offset: index_start + offset,
              raw_offset: 0,
              size,
            });
          }
          segments.index = index;
          break;
        default:
          log('Unknown segment', name);
          break;
      }
    } catch (e) {
      break;
    }
  }
  if (!segments.index) {
    throw new Error('No index');
  }
  if (!segments.metadata) {
    throw new Error('No metadata');
  }
  if (!segments.config) {
    throw new Error('No config');
  }
  log(segments);
  dfsIndexCache.set(source, {
    index: segments.index,
    metadata: segments.metadata,
    installer_end: index_start + config_sz + theme_sz,
  });
}
export const getDfsIndexCache = async (
  source: string,
  binurl: string,
): Promise<Map<string, Embedded>> => {
  if (dfsIndexCache.has(source)) {
    return dfsIndexCache.get(source)?.index as Map<string, Embedded>;
  } else {
    await refreshDfsIndex(source, binurl);
    if (dfsIndexCache.has(source)) {
      return dfsIndexCache.get(source)?.index as Map<string, Embedded>;
    }
  }
  throw new Error('No cache');
};
export const getDfsUrl = async (
  source: string,
  hash: string,
  installer?: boolean,
): Promise<{
  url: string;
  offset: number;
  size: number;
  skip_decompress?: boolean;
  skip_hash?: boolean;
}> => {
  const { remote, storage, url } = getDfsSourceType(source);

  if (storage === 'hashed') {
    const full_file_url =
      remote === 'direct'
        ? url
        : await getDfsFileUrl(dfsJsonUrlToHashed(url, hash));
    return {
      url: full_file_url,
      offset: 0,
      size: 0,
    };
  } else {
    const full_file_url = remote === 'direct' ? url : await getDfsFileUrl(url);
    const cache = await getDfsIndexCache(source, full_file_url);
    const file = cache.get(hash);
    if (!file) {
      if (installer) {
        return {
          url: full_file_url,
          offset: 0,
          size: dfsIndexCache.get(source)?.installer_end || 0,
          skip_decompress: true,
          skip_hash: true,
        };
      }
      throw new Error('No file in remote binary');
    }
    return {
      url: full_file_url,
      offset: file.offset,
      size: file.size,
    };
  }
};

export const dfsJsonUrlToHashed = (jsonUrl: string, hash: string): string => {
  // path/to/.metadata.json -> path/to/hashed/${hash}
  const url = new URL(jsonUrl);
  const path = url.pathname;
  const lastSlash = path.lastIndexOf('/');
  const dir = path.slice(0, lastSlash);
  return `${url.origin}${dir}/hashed/${hash}`;
};

export const getDfsFileUrl = async (apiurl: string): Promise<string> => {
  const dfs_result = await invoke<InvokeGetDfsRes>('get_dfs', {
    url: apiurl,
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
  dfsSource: string,
  local: Embedded[],
  source: string,
  hashKey: DfsMetadataHashType,
  item: DfsUpdateTask,
  disable_patch = false,
  disable_local = false,
  elevate = false,
) => {
  let filename_with_first_slash = item.file_name.startsWith('/')
    ? item.file_name
    : `/${item.file_name}`;
  item.downloaded = 0;
  const onProgress = ({ payload }: { payload: number }) => {
    if (isNaN(payload)) return;
    item.downloaded = payload;
  };
  item.running = true;
  try {
    const hasLocalFile = local.find((l) => l.name === item[hashKey]);
    const hasLpatchFile = local.find(
      (l) => l.name === item.lpatch?.from[hashKey],
    );
    if (hasLocalFile && !disable_local) {
      await ipc(
        InstallFile(hasLocalFile, source + filename_with_first_slash, {
          md5: item.md5,
          xxh: item.xxh,
        }),
        elevate,
        onProgress,
      );
      log('>LOCAL', filename_with_first_slash);
    } else if (
      hasLpatchFile &&
      item.lpatch &&
      !disable_patch &&
      !disable_local
    ) {
      const hash = `${item.lpatch.from[hashKey]}_${item.lpatch.to[hashKey]}`;
      const url = await getDfsUrl(dfsSource, hash);
      url.size = url.size || (item.lpatch?.size as number);
      log('>LPATCH', filename_with_first_slash, item.lpatch, url);
      await ipc(
        hybridPatch(hasLpatchFile, url, source + filename_with_first_slash, {
          md5: item.md5,
          xxh: item.xxh,
        }),
        elevate,
        onProgress,
      );
    } else if (item.patch && !disable_patch) {
      const hash = `${item.patch.from[hashKey]}_${item.patch.to[hashKey]}`;
      const url = await getDfsUrl(dfsSource, hash);
      log('>PATCH', filename_with_first_slash, item.patch, url);
      await ipc(
        InstallFile(
          url,
          source + filename_with_first_slash,
          {
            md5: item.md5,
            xxh: item.xxh,
          },
          item.patch.size,
        ),
        elevate,
        onProgress,
      );
    } else {
      const hash = item[hashKey] as string;
      const url = await getDfsUrl(dfsSource, hash, item.installer);
      log('>DOWNLOAD', filename_with_first_slash, url, item.installer);
      await ipc(
        InstallFile(url, source + filename_with_first_slash, {
          md5: item.md5,
          xxh: item.xxh,
        }),
        elevate,
        onProgress,
      );
    }
  } catch (e) {
    log(e);
    item.downloaded = 0;
    throw e;
  } finally {
    item.running = false;
  }
  item.downloaded = item.patch ? item.patch.size : item.size;
};
