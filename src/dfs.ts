import { hybridPatch, InstallFile } from './api/installFile';
import { ipc, log, warn } from './api/ipc';
import { invoke } from './tauri';

const connectableOrigins = new Set();

export const dfsIndexCache = new Map<
  string,
  {
    index: Map<string, Embedded>;
    metadata: InvokeGetDfsMetadataRes;
    installer_end: number;
    resource_version?: string; // DFS2 resource version
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
  /^(?:(dfs2?)\+)?(?:(hashed|packed|auto)\+)?(http(?:s)?:\/\/(?:.*?))$/;

export const getDfsSourceType = (
  source: string,
): {
  remote: 'dfs' | 'dfs2' | 'direct';
  storage: 'hashed' | 'packed';
  url: string;
} => {
  const match = source.match(dfsSourceReg);
  if (!match) throw new Error('Invalid dfs source: ' + source);
  const remote = ['dfs', 'dfs2', 'direct'].includes(match[1])
    ? match[1]
    : 'direct';
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
  extras?: string,
): Promise<InvokeGetDfsMetadataRes> => {
  const { remote, storage, url } = getDfsSourceType(source);

  if (remote === 'dfs2') {
    // DFS2: Use server-parsed metadata
    if (dfsIndexCache.has(source)) {
      return dfsIndexCache.get(source)?.metadata as InvokeGetDfsMetadataRes;
    }

    const dfs2Metadata = await invoke<Dfs2Metadata>('get_dfs2_metadata', {
      apiUrl: url,
    });

    if (!dfs2Metadata.data) {
      throw new Error(
        'DFS2 requires server-parsed metadata, but server returned null',
      );
    }

    // Convert DFS2 format to existing format
    const convertedIndex = new Map<string, Embedded>();
    Object.entries(dfs2Metadata.data.index).forEach(([name, info]) => {
      convertedIndex.set(name, {
        name: info.name,
        offset: info.offset,
        raw_offset: info.raw_offset,
        size: info.size,
      });
    });

    // Cache the converted data with resource version
    dfsIndexCache.set(source, {
      index: convertedIndex,
      metadata: dfs2Metadata.data.metadata as InvokeGetDfsMetadataRes,
      installer_end: dfs2Metadata.data.installer_end,
      resource_version: dfs2Metadata.resource_version, // Store version for session creation
    });

    return dfs2Metadata.data.metadata as InvokeGetDfsMetadataRes;
  } else {
    // DFS1: Use existing client-side parsing logic
    if (storage === 'hashed') {
      // Existing hashed logic would go here
      throw new Error('Hashed storage not implemented');
    } else {
      if (dfsIndexCache.has(source)) {
        return dfsIndexCache.get(source)?.metadata as InvokeGetDfsMetadataRes;
      } else {
        await refreshDfsIndex(source, url, remote, extras);
        if (dfsIndexCache.has(source)) {
          return dfsIndexCache.get(source)?.metadata as InvokeGetDfsMetadataRes;
        }
      }
    }
  }
  throw new Error('Get metadata failed');
};
export async function refreshDfsIndex(
  source: string,
  apiurl: string,
  remote: 'direct' | 'dfs' | 'dfs2',
  extras?: string,
) {
  const binurl =
    remote === 'direct' ? apiurl : await getDfsFileUrl(apiurl, extras, 256);
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
        case '\0INDEX': {
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
        }
        default:
          log('Unknown segment', name);
          break;
      }
    } catch {
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
  apiurl: string,
  remote: 'direct' | 'dfs' | 'dfs2',
): Promise<Map<string, Embedded>> => {
  if (dfsIndexCache.has(source)) {
    return dfsIndexCache.get(source)?.index as Map<string, Embedded>;
  } else {
    await refreshDfsIndex(source, apiurl, remote);
    if (dfsIndexCache.has(source)) {
      return dfsIndexCache.get(source)?.index as Map<string, Embedded>;
    }
  }
  throw new Error('No cache');
};
export const getDfsUrl = async (
  source: string,
  hash: string,
  extras?: string,
  installer?: boolean,
): Promise<{
  url: string;
  offset: number;
  size: number;
  skip_decompress?: boolean;
  skip_hash?: boolean;
}> => {
  const { remote, storage, url } = getDfsSourceType(source);

  if (remote === 'dfs2') {
    // DFS2: Use session-based download
    const cache = dfsIndexCache.get(source);
    if (!cache) {
      throw new Error('DFS2 metadata not loaded');
    }

    const file = cache.index.get(hash);
    if (!file) {
      if (installer) {
        // For installer, get the installer portion
        const range = `0-${cache.installer_end - 1}`;
        const cdnUrl = await getDfs2Url(url, range);
        return {
          url: cdnUrl,
          offset: 0,
          size: cache.installer_end,
          skip_decompress: true,
        };
      }
      throw new Error('No file in DFS2 index');
    }

    // Get specific file range
    const range = `${file.offset}-${file.offset + file.size - 1}`;
    const cdnUrl = await getDfs2Url(url, range);
    return {
      url: cdnUrl,
      offset: file.offset,
      size: file.size,
    };
  } else {
    // DFS1: Use existing logic
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
      const end = dfsIndexCache.get(source)?.installer_end || 0;
      const cache = await getDfsIndexCache(source, url, remote);
      const file = cache.get(hash);
      if (!file) {
        if (installer) {
          const full_file_url =
            remote === 'direct' ? url : await getDfsFileUrl(url, extras, end);
          return {
            url: full_file_url,
            offset: 0,
            size: end,
            skip_decompress: true,
          };
        }
        throw new Error('No file in remote binary');
      }
      const full_file_url =
        remote === 'direct'
          ? url
          : await getDfsFileUrl(url, extras, file.size, file.offset);
      return {
        url: full_file_url,
        offset: file.offset,
        size: file.size,
      };
    }
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

export const getDfsFileUrl = async (
  apiurl: string,
  extras?: string,
  length?: number,
  start = 0,
): Promise<string> => {
  const dfs_result = await invoke<InvokeGetDfsRes>('get_dfs', {
    url: apiurl,
    extras: extras || undefined,
    range: length ? `${start}-${start + length - 1}` : undefined,
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

// DFS2 Session Management Functions
export const createDfs2Session = async (
  apiUrl: string,
  chunks?: string[],
  version?: string,
  extras?: string,
): Promise<string> => {
  // Parse extras string to JSON object if provided
  let extrasObject: unknown = undefined;
  if (extras && extras.trim() !== '') {
    try {
      extrasObject = JSON.parse(extras);
    } catch (e) {
      throw new Error(`Invalid extras JSON format: ${e}`);
    }
  }

  let challengeResponse: string | undefined = undefined;
  let sessionId: string | undefined = undefined;

  // Challenge handling loop
  for (let attempts = 0; attempts < 3; attempts++) {
    const sessionResponse: Dfs2SessionResponse =
      await invoke<Dfs2SessionResponse>('create_dfs2_session', {
        apiUrl: apiUrl,
        chunks: chunks || undefined,
        version: version || undefined,
        challengeResponse: challengeResponse,
        sessionId: sessionId,
        extras: extrasObject,
      });

    // Success - session created
    if (sessionResponse.sid && !sessionResponse.challenge) {
      // Store session for cleanup
      storeDfs2Session(apiUrl, sessionResponse.sid);
      return sessionResponse.sid;
    }

    // Challenge received
    if (
      sessionResponse.challenge &&
      sessionResponse.data &&
      sessionResponse.sid
    ) {
      console.log(`DFS2 challenge received: ${sessionResponse.challenge}`);

      try {
        if (sessionResponse.challenge === 'web') {
          // Handle web challenges
          challengeResponse = await handleWebChallenge(sessionResponse.data);
        } else {
          // Handle computational challenges (MD5, SHA256)
          challengeResponse = await invoke<string>('solve_dfs2_challenge', {
            challengeType: sessionResponse.challenge,
            data: sessionResponse.data,
          });
        }

        sessionId = sessionResponse.sid;
        console.log('Challenge solved, retrying session creation...');
        continue;
      } catch (error) {
        throw new Error(
          `Failed to solve ${sessionResponse.challenge} challenge: ${error}`,
        );
      }
    }

    // Unexpected response
    throw new Error('Invalid session response format');
  }

  throw new Error('Failed to create session after 3 challenge attempts');
};

// Web Challenge Handler
const handleWebChallenge = async (challengeData: string): Promise<string> => {
  // TODO: Implement web challenge handling
  // This could involve:
  // - Opening a popup window
  // - Handling captcha
  // - User authentication
  // - Redirect flows
  //
  // For now, throw an error to indicate it needs implementation
  throw new Error(
    'Web challenges not implemented yet. Challenge data: ' + challengeData,
  );
};

// DFS2 session cache - now only stores sessions created in runInstall
const dfs2Sessions = new Map<
  string,
  { sessionId: string; baseUrl: string; resId: string }
>();

// Store DFS2 session info after creation in runInstall
export const storeDfs2Session = (apiUrl: string, sessionId: string): void => {
  const url = new URL(apiUrl);
  const baseUrl = `${url.protocol}//${url.host}`;
  const resId = url.pathname.split('/').pop() || '';

  dfs2Sessions.set(apiUrl, {
    sessionId,
    baseUrl,
    resId,
  });
};

// Clean up specific DFS2 session
export const cleanupDfs2Session = async (apiUrl: string): Promise<void> => {
  const sessionInfo = dfs2Sessions.get(apiUrl);
  if (sessionInfo) {
    try {
      log('Ending DFS2 session:', sessionInfo.sessionId);
      const sessionApiUrl = `${sessionInfo.baseUrl}/session/${sessionInfo.sessionId}/${sessionInfo.resId}`;
      await invoke('end_dfs2_session', {
        sessionApiUrl: sessionApiUrl,
        insights: undefined,
      });
      log('DFS2 session ended successfully:', sessionInfo.sessionId);
    } catch (error) {
      warn('Failed to end DFS2 session:', sessionInfo.sessionId, error);
    }
    dfs2Sessions.delete(apiUrl);
  }
};

// Clean up all DFS2 sessions (call at end of installation)
export const cleanupAllDfs2Sessions = async (): Promise<void> => {
  const cleanupPromises: Promise<void>[] = [];

  for (const apiUrl of dfs2Sessions.keys()) {
    cleanupPromises.push(cleanupDfs2Session(apiUrl));
  }

  // Wait for all sessions to be cleaned up
  await Promise.allSettled(cleanupPromises);

  // Clear the cache
  dfs2Sessions.clear();
};

// DFS2-specific URL getter using pre-created session
export const getDfs2Url = async (
  apiUrl: string,
  range: string,
): Promise<string> => {
  const sessionInfo = dfs2Sessions.get(apiUrl);
  if (!sessionInfo) {
    throw new Error(
      'DFS2 session not found - session must be created in runInstall',
    );
  }

  const sessionApiUrl = `${sessionInfo.baseUrl}/session/${sessionInfo.sessionId}/${sessionInfo.resId}`;
  const response = await invoke<Dfs2ChunkResponse>('get_dfs2_chunk_url', {
    sessionApiUrl: sessionApiUrl,
    range,
  });

  return response.url;
};

// Collect ranges needed for DFS2 session creation
export const collectDfs2Ranges = (
  diffFiles: DfsUpdateTask[],
  localFiles: Embedded[],
  dfsSource: string,
  hashKey: DfsMetadataHashType,
): string[] => {
  const { remote } = getDfsSourceType(dfsSource);
  if (remote !== 'dfs2') {
    return [];
  }

  const cache = dfsIndexCache.get(dfsSource);
  if (!cache) {
    throw new Error('DFS2 metadata not loaded');
  }

  const ranges = new Set<string>();

  diffFiles.forEach((item) => {
    const hasLocalFile = localFiles.find((l) => l.name === item[hashKey]);
    const hasLpatchFile =
      item.lpatch &&
      localFiles.find((l) => l.name === item.lpatch?.from[hashKey]);

    if (hasLocalFile) {
      // Skip: has local file, no need to download
      return;
    }

    if (item.lpatch && hasLpatchFile) {
      // Lpatch mode: need both lpatch file and original file ranges
      // 1. Lpatch file range
      const lpatchHash = `${item.lpatch.from[hashKey]}_${item.lpatch.to[hashKey]}`;
      const lpatchFile = cache.index.get(lpatchHash);
      if (lpatchFile) {
        ranges.add(
          `${lpatchFile.offset}-${lpatchFile.offset + lpatchFile.size - 1}`,
        );
      }

      // 2. Original file range (fallback)
      const originalFile = cache.index.get(item[hashKey] as string);
      if (originalFile) {
        ranges.add(
          `${originalFile.offset}-${originalFile.offset + originalFile.size - 1}`,
        );
      }
    } else if (item.patch) {
      // Patch mode: need both patch file and original file ranges
      // 1. Patch file range
      const patchHash = `${item.patch.from[hashKey]}_${item.patch.to[hashKey]}`;
      const patchFile = cache.index.get(patchHash);
      if (patchFile) {
        ranges.add(
          `${patchFile.offset}-${patchFile.offset + patchFile.size - 1}`,
        );
      }

      // 2. Original file range (fallback)
      const originalFile = cache.index.get(item[hashKey] as string);
      if (originalFile) {
        ranges.add(
          `${originalFile.offset}-${originalFile.offset + originalFile.size - 1}`,
        );
      }
    } else {
      // Normal download: only need the file itself
      const file = cache.index.get(item[hashKey] as string);
      if (file) {
        ranges.add(`${file.offset}-${file.offset + file.size - 1}`);
      }
    }

    // Handle installer files
    if (item.installer && cache.installer_end > 0) {
      ranges.add(`0-${cache.installer_end - 1}`);
    }
  });

  return Array.from(ranges);
};

export const runDfsDownload = async (
  dfsSource: string,
  extras: string | undefined,
  local: Embedded[],
  source: string,
  hashKey: DfsMetadataHashType,
  item: DfsUpdateTask,
  disable_patch = false,
  disable_local = false,
  elevate = false,
) => {
  const filename_with_first_slash = item.file_name.startsWith('/')
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
        InstallFile(
          hasLocalFile,
          source + filename_with_first_slash,
          {
            md5: item.md5,
            xxh: item.xxh,
          },
          undefined,
          item.installer,
        ),
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
          item.installer,
        ),
        elevate,
        onProgress,
      );
    } else {
      const hash = item[hashKey] as string;
      const url = await getDfsUrl(dfsSource, hash, extras, item.installer);
      log('>DOWNLOAD', filename_with_first_slash, url, item.installer);
      await ipc(
        InstallFile(
          url,
          source + filename_with_first_slash,
          {
            md5: item.md5,
            xxh: item.xxh,
          },
          undefined,
          item.installer,
        ),
        elevate,
        onProgress,
      );
    }
  } catch (e) {
    warn(e);
    item.downloaded = 0;
    throw e;
  } finally {
    item.running = false;
  }
  item.downloaded = item.patch ? item.patch.size : item.size;
};
