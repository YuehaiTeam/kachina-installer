import { hybridPatch, InstallFile } from './api/installFile';
import { ipc, log, warn, addInsightWithMode } from './api/ipc';
import { invoke } from './tauri';
import { clearNetworkInsights } from './networkInsights';
import { KachinaInstallSource, pluginManager } from './plugins';
import { registerAllPlugins } from './plugins/registry';
import {
  Dfs2BatchChunkResponse,
  Dfs2Metadata,
  Dfs2SessionResponse,
  DfsMetadataHashType,
  DfsUpdateTask,
  Embedded,
  FileWithPosition,
  InsightItem,
  InvokeGetDfsMetadataRes,
  InvokeGetDfsRes,
  MergedGroupInfo,
  ProjectConfig,
  TAError,
  VirtualMergedFile,
} from './types';

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

// 初始化插件系统
registerAllPlugins();

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
  /^(?:(dfs2?)\+)?(?:(hashed|packed|auto)\+)?(?:plugin-[^+]+\+)?(http(?:s)?:\/\/(?:.*?))$/;

export const getDfsSourceType = (
  source: string,
): {
  remote: 'dfs' | 'dfs2' | 'direct' | 'plugin';
  storage: 'hashed' | 'packed';
  url: string;
  plugin?: KachinaInstallSource;
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

  // 检查解析出的URL是否匹配插件
  const plugin = pluginManager.findPlugin(source);
  if (plugin) {
    return {
      remote: 'plugin' as const,
      storage: storage as 'hashed' | 'packed',
      url: match[3],
      plugin,
    };
  }

  return {
    remote: remote as 'dfs' | 'dfs2' | 'direct',
    storage: storage as 'hashed' | 'packed',
    url: match[3],
  };
};
export const getDfsMetadata = async (
  source: string,
  extras?: string,
): Promise<InvokeGetDfsMetadataRes> => {
  // 检查插件源
  const { remote, storage, url, plugin } = getDfsSourceType(source);
  if (plugin) {
    if (plugin.getMetadata) {
      // 插件提供了自定义元数据获取
      if (dfsIndexCache.has(source)) {
        return dfsIndexCache.get(source)?.metadata as InvokeGetDfsMetadataRes;
      }

      const cleanUrl = pluginManager.getCleanUrl(source);
      if (!cleanUrl) throw new Error('Invalid plugin URL: ' + source);
      const dfs2Data = await plugin.getMetadata(cleanUrl);

      // 转换为现有格式并缓存
      const convertedIndex = new Map<string, Embedded>();
      Object.entries(dfs2Data.index).forEach(([name, info]) => {
        convertedIndex.set(name, {
          name: info.name,
          offset: info.offset,
          raw_offset: info.raw_offset,
          size: info.size,
        });
      });

      dfsIndexCache.set(source, {
        index: convertedIndex,
        metadata: dfs2Data.metadata,
        installer_end: dfs2Data.installer_end,
      });

      return dfs2Data.metadata;
    } else {
      // 插件没提供元数据获取，使用标准packed逻辑
      if (dfsIndexCache.has(source)) {
        return dfsIndexCache.get(source)?.metadata as InvokeGetDfsMetadataRes;
      } else {
        await refreshDfsIndex(source, source, remote, extras);
        return dfsIndexCache.get(source)?.metadata as InvokeGetDfsMetadataRes;
      }
    }
  }

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
  remote: 'direct' | 'dfs' | 'dfs2' | 'plugin',
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
  const index: [number, number[]] = await invoke('get_http_with_range', {
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
  remote: 'direct' | 'dfs' | 'dfs2' | 'plugin',
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
  const { remote, storage, url, plugin } = getDfsSourceType(source);

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
            remote === 'direct'
              ? url
              : await getDfsFileUrl(plugin ? source : url, extras, end);
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
          : await getDfsFileUrl(
              remote === 'plugin' ? source : url,
              extras,
              file.size,
              file.offset,
            );
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
  // 检查是否为插件源
  const plugin = pluginManager.findPlugin(apiurl);
  if (plugin) {
    const cleanUrl = pluginManager.getCleanUrl(apiurl);
    if (!cleanUrl) throw new Error('Invalid plugin URL: ' + apiurl);

    const range = length ? `${start}-${start + length - 1}` : '';
    const result = await plugin.getChunkUrl(cleanUrl, range);
    return result.url;
  }

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

// Helper function to check if error is a network error that should be retried
const isNetworkError = (error: unknown): boolean => {
  const errorStr = JSON.stringify(error);

  // 检查是否为HTTP状态码错误（4xx/5xx），这些不应该重试
  if (errorStr.includes('Session creation failed:')) {
    return false; // HTTP状态错误，不重试
  }

  // 检查 reqwest/hyper 网络库错误结构（这些需要重试）
  if (
    errorStr.includes('Failed to send request:') &&
    (errorStr.includes('reqwest::Error') ||
      errorStr.includes('hyper::Error') ||
      errorStr.includes('hyper_util::client::legacy::Error'))
  ) {
    return true;
  }

  // 检查连接相关的 kind 字段（英文，不会本地化）
  if (
    errorStr.includes('kind: ConnectionReset') ||
    errorStr.includes('kind: Timeout') ||
    errorStr.includes('kind: ConnectionRefused') ||
    errorStr.includes('kind: NotFound')
  ) {
    return true;
  }

  // 检查常见的网络相关错误码
  if (/code: (10054|10060|10061)/.test(errorStr)) {
    return true;
  }

  return false;
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

  // Main retry loop with network error handling (max 3 retries)
  const retryIntervals = [200, 600, 1000]; // 0.2s, 0.6s, 1s

  for (let retryAttempt = 0; retryAttempt < 3; retryAttempt++) {
    let challengeResponse: string | undefined = undefined;
    let sessionId: string | undefined = undefined;

    try {
      // Challenge handling loop (max 3 challenge attempts per retry)
      for (
        let challengeAttempts = 0;
        challengeAttempts < 3;
        challengeAttempts++
      ) {
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
              // Handle web challenges - failure should exit immediately
              challengeResponse = await handleWebChallenge(
                sessionResponse.data,
              );
            } else {
              // Handle computational challenges (MD5, SHA256)
              challengeResponse = await invoke<string>('solve_dfs2_challenge', {
                challengeType: sessionResponse.challenge,
                data: sessionResponse.data,
              });
            }

            sessionId = sessionResponse.sid;
            console.log('Challenge solved, retrying session creation...');
            continue; // Continue challenge handling loop
          } catch (error) {
            if (sessionResponse.challenge === 'web') {
              // Web challenge failure should exit immediately
              throw new Error(`Web challenge failed: ${error}`);
            } else {
              // Non-web challenge failure should retry challenge
              console.warn(
                `Challenge ${sessionResponse.challenge} failed, retrying...`,
              );
              // Reset challenge data for retry
              challengeResponse = undefined;
              sessionId = undefined;
              continue; // Continue challenge handling loop for retry
            }
          }
        }

        // Unexpected response
        throw new Error('Invalid session response format');
      }

      // If we get here, challenge attempts exceeded
      throw new Error('Failed to create session after 3 challenge attempts');
    } catch (error) {
      // Check if this is a network error and we have retries left
      if (isNetworkError(error) && retryAttempt < 2) {
        console.warn(
          `Network error on attempt ${retryAttempt + 1}, retrying in ${retryIntervals[retryAttempt]}ms...`,
          error,
        );
        // Add delay before retry
        await new Promise((resolve) =>
          setTimeout(resolve, retryIntervals[retryAttempt]),
        );
        continue; // Continue main retry loop
      }

      // Web challenge failures or non-network errors should not retry
      if (
        error instanceof Error &&
        error.message.includes('Web challenge failed')
      ) {
        throw error;
      }

      // If not a network error or out of retries, throw the error
      if (retryAttempt === 2) {
        throw new Error(
          `Failed to create DFS2 session after ${retryAttempt + 1} attempts: ${error}`,
        );
      }

      throw error;
    }
  }

  // This should never be reached, but just in case
  throw new Error('Failed to create session: unexpected exit from retry loop');
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
export const cleanupDfs2Session = async (
  apiUrl: string,
  serversSnapshot?: InsightItem[],
): Promise<void> => {
  const sessionInfo = dfs2Sessions.get(apiUrl);
  if (sessionInfo) {
    try {
      log('Ending DFS2 session:', sessionInfo.sessionId);
      const sessionApiUrl = `${sessionInfo.baseUrl}/session/${sessionInfo.sessionId}/${sessionInfo.resId}`;

      // 使用快照上报数据，防止被并发修改
      // 始终结束 session，如有快照则带上 insights
      await invoke('end_dfs2_session', {
        sessionApiUrl: sessionApiUrl,
        insights: serversSnapshot ? { servers: serversSnapshot } : undefined,
      });
      log('DFS2 session ended successfully:', sessionInfo.sessionId);
    } catch (error) {
      warn('Failed to end DFS2 session:', sessionInfo.sessionId, error);
    }
    dfs2Sessions.delete(apiUrl);
  }
};

// Clean up all DFS2 sessions (call at end of installation)
export const cleanupAllDfs2Sessions = async (
  serversSnapshot?: InsightItem[],
): Promise<void> => {
  const cleanupPromises: Promise<void>[] = [];

  for (const apiUrl of dfs2Sessions.keys()) {
    cleanupPromises.push(cleanupDfs2Session(apiUrl, serversSnapshot));
  }

  // 等待所有session清理完成
  await Promise.allSettled(cleanupPromises);

  // 清空统计数据（在所有上报完成后）
  // 只有提供快照时才清空（表示是正常的安装结束流程）
  if (serversSnapshot) {
    clearNetworkInsights();
  }

  // 清理session缓存
  dfs2Sessions.clear();
};

// Chunk URL request aggregator for batch processing
interface PendingRequest {
  resolve: (url: string) => void;
  reject: (error: Error) => void;
}

class ChunkUrlAggregator {
  private static instance: ChunkUrlAggregator;
  private pendingRequests = new Map<string, PendingRequest[]>(); // range -> requests[]
  private aggregationTimer: ReturnType<typeof setTimeout> | null = null;
  private currentSessionUrl: string | null = null;
  private readonly AGGREGATION_WINDOW_MS = 50;

  static getInstance(): ChunkUrlAggregator {
    if (!this.instance) {
      this.instance = new ChunkUrlAggregator();
    }
    return this.instance;
  }

  async requestChunkUrl(sessionUrl: string, range: string): Promise<string> {
    return new Promise((resolve, reject) => {
      // 添加到pending requests
      if (!this.pendingRequests.has(range)) {
        this.pendingRequests.set(range, []);
      }
      this.pendingRequests.get(range)!.push({ resolve, reject });

      // 设置或重置聚合定时器
      this.currentSessionUrl = sessionUrl;
      this.scheduleAggregation();
    });
  }

  private scheduleAggregation(): void {
    if (this.aggregationTimer) {
      clearTimeout(this.aggregationTimer);
    }

    this.aggregationTimer = setTimeout(() => {
      this.flushPendingRequests();
    }, this.AGGREGATION_WINDOW_MS);
  }

  private async flushPendingRequests(): Promise<void> {
    if (this.pendingRequests.size === 0 || !this.currentSessionUrl) {
      return;
    }

    const sessionUrl = this.currentSessionUrl;
    const requestsToProcess = new Map(this.pendingRequests);

    // 清空待处理队列
    this.pendingRequests.clear();
    this.aggregationTimer = null;
    this.currentSessionUrl = null;

    try {
      // 批量请求
      const chunks = Array.from(requestsToProcess.keys());
      log(
        `[ChunkAggregator] Batching ${chunks.length} chunks: ${chunks.join(',')}`,
      );

      const response = await invoke<Dfs2BatchChunkResponse>(
        'get_dfs2_batch_chunk_urls',
        {
          sessionApiUrl: sessionUrl,
          chunks: chunks,
        },
      );

      // 分发结果
      for (const [range, pendingRequests] of requestsToProcess) {
        const result = response.urls[range];

        if (result?.url) {
          // 成功：通知所有等待该range的请求
          pendingRequests.forEach((req) => req.resolve(result.url!));
        } else {
          // 失败：通知错误
          const error = new Error(result?.error || 'Unknown error');
          pendingRequests.forEach((req) => req.reject(error));
        }
      }
    } catch (error) {
      // 批量请求失败：通知所有pending requests
      for (const pendingRequests of requestsToProcess.values()) {
        pendingRequests.forEach((req) => req.reject(error as Error));
      }
    }
  }
}

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

  // 使用聚合器获取URL
  const aggregator = ChunkUrlAggregator.getInstance();
  return await aggregator.requestChunkUrl(sessionApiUrl, range);
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

  // 先预处理文件，获取合并信息
  const { processedFiles } = preprocessFiles(
    diffFiles,
    dfsSource,
    hashKey,
    localFiles,
  );

  const ranges = new Set<string>();

  processedFiles.forEach((item) => {
    if ((item as VirtualMergedFile)._isMergedGroup) {
      // 合并组：添加合并后的range
      const virtualFile = item as VirtualMergedFile;
      ranges.add(virtualFile._mergedInfo.mergedRange);

      // 同时添加原始文件的ranges作为fallback
      virtualFile._mergedInfo.files.forEach((originalFile) => {
        const hasLocalFile = localFiles.find(
          (l) => l.name === originalFile[hashKey],
        );
        const hasLpatchFile =
          originalFile.lpatch &&
          localFiles.find((l) => l.name === originalFile.lpatch?.from[hashKey]);

        if (hasLocalFile) {
          // Skip: has local file, no need to download
          return;
        }

        if (originalFile.lpatch && hasLpatchFile) {
          // Lpatch mode: need both lpatch file and original file ranges
          const lpatchHash = `${originalFile.lpatch.from[hashKey]}_${originalFile.lpatch.to[hashKey]}`;
          const lpatchFile = cache.index.get(lpatchHash);
          if (lpatchFile) {
            ranges.add(
              `${lpatchFile.offset}-${lpatchFile.offset + lpatchFile.size - 1}`,
            );
          }
          const originalDfsFile = cache.index.get(
            originalFile[hashKey] as string,
          );
          if (originalDfsFile) {
            ranges.add(
              `${originalDfsFile.offset}-${originalDfsFile.offset + originalDfsFile.size - 1}`,
            );
          }
        } else if (originalFile.patch) {
          // Patch mode: need both patch file and original file ranges
          const patchHash = `${originalFile.patch.from[hashKey]}_${originalFile.patch.to[hashKey]}`;
          const patchFile = cache.index.get(patchHash);
          if (patchFile) {
            ranges.add(
              `${patchFile.offset}-${patchFile.offset + patchFile.size - 1}`,
            );
          }
          const originalDfsFile = cache.index.get(
            originalFile[hashKey] as string,
          );
          if (originalDfsFile) {
            ranges.add(
              `${originalDfsFile.offset}-${originalDfsFile.offset + originalDfsFile.size - 1}`,
            );
          }
        } else {
          // Normal download: only need the file itself
          const file = cache.index.get(originalFile[hashKey] as string);
          if (file) {
            ranges.add(`${file.offset}-${file.offset + file.size - 1}`);
          }
        }

        // Handle installer files
        if (originalFile.installer && cache.installer_end > 0) {
          ranges.add(`0-${cache.installer_end - 1}`);
        }
      });
    } else {
      // 普通文件：按原有逻辑处理
      const dfsFile = item as DfsUpdateTask;
      const hasLocalFile = localFiles.find((l) => l.name === dfsFile[hashKey]);
      const hasLpatchFile =
        dfsFile.lpatch &&
        localFiles.find((l) => l.name === dfsFile.lpatch?.from[hashKey]);

      if (hasLocalFile) {
        // Skip: has local file, no need to download
        return;
      }

      if (dfsFile.lpatch && hasLpatchFile) {
        // Lpatch mode: need both lpatch file and original file ranges
        const lpatchHash = `${dfsFile.lpatch.from[hashKey]}_${dfsFile.lpatch.to[hashKey]}`;
        const lpatchFile = cache.index.get(lpatchHash);
        if (lpatchFile) {
          ranges.add(
            `${lpatchFile.offset}-${lpatchFile.offset + lpatchFile.size - 1}`,
          );
        }
        const originalFile = cache.index.get(dfsFile[hashKey] as string);
        if (originalFile) {
          ranges.add(
            `${originalFile.offset}-${originalFile.offset + originalFile.size - 1}`,
          );
        }
      } else if (dfsFile.patch) {
        // Patch mode: need both patch file and original file ranges
        const patchHash = `${dfsFile.patch.from[hashKey]}_${dfsFile.patch.to[hashKey]}`;
        const patchFile = cache.index.get(patchHash);
        if (patchFile) {
          ranges.add(
            `${patchFile.offset}-${patchFile.offset + patchFile.size - 1}`,
          );
        }
        const originalFile = cache.index.get(dfsFile[hashKey] as string);
        if (originalFile) {
          ranges.add(
            `${originalFile.offset}-${originalFile.offset + originalFile.size - 1}`,
          );
        }
      } else {
        // Normal download: only need the file itself
        const file = cache.index.get(dfsFile[hashKey] as string);
        if (file) {
          ranges.add(`${file.offset}-${file.offset + file.size - 1}`);
        }
      }

      // Handle installer files
      if (dfsFile.installer && cache.installer_end > 0) {
        ranges.add(`0-${cache.installer_end - 1}`);
      }
    }
  });

  const rangeArray = Array.from(ranges);
  log('DFS2 ranges collected:', {
    totalRanges: rangeArray.length,
    mergedGroups: processedFiles.filter(
      (f) => (f as VirtualMergedFile)._isMergedGroup,
    ).length,
    ranges: rangeArray,
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
): Promise<{ insight?: InsightItem }> => {
  const filename_with_first_slash = item.file_name.startsWith('/')
    ? item.file_name
    : `/${item.file_name}`;
  item.downloaded = 0;
  const onProgress = ({ payload }: { payload: number }) => {
    if (isNaN(payload)) return;
    item.downloaded = payload;
  };
  item.running = true;
  const hasLocalFile = local.find((l) => l.name === item[hashKey]);
  const hasLpatchFile = local.find(
    (l) => l.name === item.lpatch?.from[hashKey],
  );

  // Track insight for return
  let collectedInsight: InsightItem | undefined = undefined;

  try {
    if (hasLocalFile && !disable_local) {
      // Local files don't involve network downloads, so no insight collection
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
      // Local: no insight
    } else if (
      hasLpatchFile &&
      item.lpatch &&
      !disable_patch &&
      !disable_local
    ) {
      // HybridPatch: collect insights with 'hybridpatch' mode
      const hash = `${item.lpatch.from[hashKey]}_${item.lpatch.to[hashKey]}`;
      const url = await getDfsUrl(dfsSource, hash);
      url.size = url.size || (item.lpatch?.size as number);
      const result: {
        insight?: InsightItem;
      } = await ipc(
        hybridPatch(hasLpatchFile, url, source + filename_with_first_slash, {
          md5: item.md5,
          xxh: item.xxh,
        }),
        elevate,
        onProgress,
      );
      if (result.insight) {
        addInsightWithMode(result.insight, 'hybridpatch');
        collectedInsight = result.insight;
      }
    } else if (item.patch && !disable_patch) {
      // Patch: collect insights with 'patch' mode
      const hash = `${item.patch.from[hashKey]}_${item.patch.to[hashKey]}`;
      const url = await getDfsUrl(dfsSource, hash);
      const result: {
        insight?: InsightItem;
      } = await ipc(
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
      if (result.insight) {
        addInsightWithMode(result.insight, 'patch');
        collectedInsight = result.insight;
      }
    } else {
      // Direct: collect insights with 'direct' mode
      const hash = item[hashKey] as string;
      const url = await getDfsUrl(dfsSource, hash, extras, item.installer);
      const result: {
        insight?: InsightItem;
      } = await ipc(
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
      if (result.insight) {
        addInsightWithMode(result.insight, 'direct');
        collectedInsight = result.insight;
      }
    }
  } catch (e) {
    item.downloaded = 0;

    // Handle error insights for network operations
    if (e instanceof TAError && e.insight) {
      let mode: string | undefined;
      if (hasLocalFile && !disable_local) {
        // Local files don't collect insights
        mode = undefined;
      } else if (
        hasLpatchFile &&
        item.lpatch &&
        !disable_patch &&
        !disable_local
      ) {
        mode = 'hybridpatch';
      } else if (item.patch && !disable_patch) {
        mode = 'patch';
      } else {
        mode = 'direct';
      }

      if (mode) {
        addInsightWithMode(e.insight, mode);
      }
      // Capture error insight for return (will be thrown, but caller can catch)
      collectedInsight = e.insight;
    }

    throw e;
  } finally {
    item.running = false;
  }
  item.downloaded = item.patch ? item.patch.size : item.size;
  return { insight: collectedInsight };
};

// 小文件合并下载相关函数

const createSingleFileGroup = (file: FileWithPosition): MergedGroupInfo => {
  return {
    files: [file],
    mergedRange: `${file.dfsOffset}-${file.dfsOffset + file.dfsSize - 1}`,
    totalDownloadSize: file.dfsSize,
    totalEffectiveSize: file.dfsSize,
    wasteRatio: 0,
    gaps: [],
  };
};

const createMergedGroup = (files: FileWithPosition[]): MergedGroupInfo => {
  if (files.length === 0) {
    throw new Error('Cannot create merged group from empty file list');
  }

  files.sort((a, b) => a.dfsOffset - b.dfsOffset);

  const firstFile = files[0];
  const lastFile = files[files.length - 1];
  const groupStart = firstFile.dfsOffset;
  const groupEnd = lastFile.dfsOffset + lastFile.dfsSize;

  const totalDownloadSize = groupEnd - groupStart;
  const totalEffectiveSize = files.reduce((sum, f) => sum + f.dfsSize, 0);
  const wasteRatio =
    (totalDownloadSize - totalEffectiveSize) / totalDownloadSize;

  // 计算gaps
  const gaps: Array<{ start: number; end: number }> = [];
  for (let i = 0; i < files.length - 1; i++) {
    const currentEnd = files[i].dfsOffset + files[i].dfsSize;
    const nextStart = files[i + 1].dfsOffset;
    if (nextStart > currentEnd) {
      gaps.push({ start: currentEnd, end: nextStart });
    }
  }

  return {
    files,
    mergedRange: `${groupStart}-${groupEnd - 1}`,
    totalDownloadSize,
    totalEffectiveSize,
    wasteRatio,
    gaps,
  };
};

const canMergeToGroup = (
  group: FileWithPosition[],
  newFile: FileWithPosition,
): boolean => {
  if (group.length === 0) return true;

  const lastFile = group[group.length - 1];
  const groupStart = group[0].dfsOffset;
  const groupEnd = lastFile.dfsOffset + lastFile.dfsSize;
  const newEnd = newFile.dfsOffset + newFile.dfsSize;

  // 检查是否有重叠
  if (newFile.dfsOffset < groupEnd) return false;

  // 计算合并后的总大小和有效大小
  const totalSize = newEnd - groupStart;
  const effectiveSize =
    group.reduce((sum, f) => sum + f.dfsSize, 0) + newFile.dfsSize;
  const wasteRatio = (totalSize - effectiveSize) / totalSize;

  // 检查约束条件
  return (
    totalSize <= 10 * 1024 * 1024 && // 不超过10MB
    wasteRatio <= 0.2
  ); // 浪费率不超过20%
};

export const mergeSmallFilesIntoGroups = (
  files: DfsUpdateTask[],
  dfsSource: string,
  hashKey: DfsMetadataHashType,
): MergedGroupInfo[] => {
  // 获取DFS缓存信息
  const cache = dfsIndexCache.get(dfsSource);
  if (!cache) {
    log('No DFS cache found, treating each file as individual group');
    return files.map((f) =>
      createSingleFileGroup({
        ...f,
        dfsOffset: 0,
        dfsSize: f.size,
      }),
    );
  }

  // 只处理小文件（≤500KB）
  const smallFiles = files.filter((f) => f.size <= 500 * 1024);
  const largeFiles = files.filter((f) => f.size > 500 * 1024);

  // 获取小文件在DFS中的实际位置和大小
  const filesWithPositions: FileWithPosition[] = smallFiles
    .map((f) => {
      const dfsFile = cache.index.get(f[hashKey] as string);
      return {
        ...f,
        dfsOffset: dfsFile?.offset || 0,
        dfsSize: dfsFile?.size || f.size,
      };
    })
    .sort((a, b) => a.dfsOffset - b.dfsOffset);

  // 贪心合并算法
  const groups: MergedGroupInfo[] = [];
  let currentGroup: FileWithPosition[] = [];

  for (const file of filesWithPositions) {
    if (canMergeToGroup(currentGroup, file)) {
      currentGroup.push(file);
    } else {
      if (currentGroup.length > 1) {
        // 只有多个文件才值得合并
        groups.push(createMergedGroup(currentGroup));
      } else if (currentGroup.length === 1) {
        // 单文件直接作为独立组
        groups.push(createSingleFileGroup(currentGroup[0]));
      }
      currentGroup = [file];
    }
  }

  // 处理最后一组
  if (currentGroup.length > 1) {
    groups.push(createMergedGroup(currentGroup));
  } else if (currentGroup.length === 1) {
    groups.push(createSingleFileGroup(currentGroup[0]));
  }

  // 将大文件也作为独立组添加
  largeFiles.forEach((f) => {
    const dfsFile = cache.index.get(f[hashKey] as string);
    groups.push(
      createSingleFileGroup({
        ...f,
        dfsOffset: dfsFile?.offset || 0,
        dfsSize: dfsFile?.size || f.size,
      }),
    );
  });

  log('File grouping result:', {
    totalFiles: files.length,
    smallFiles: smallFiles.length,
    largeFiles: largeFiles.length,
    groups: groups.length,
    mergedGroups: groups.filter((g) => g.files.length > 1).length,
  });

  return groups;
};

// Helper function to identify file install mode
export const getFileInstallMode = (
  file: DfsUpdateTask,
  local: Embedded[],
  hashKey: DfsMetadataHashType,
): 'local' | 'hybridpatch' | 'patch' | 'direct' => {
  if (file.failed) {
    return 'direct'; // Failed files should always be retried as direct downloads
  }
  const hasLocalFile = local.find((l) => l.name === file[hashKey]);
  const hasLpatchFile = local.find(
    (l) => l.name === file.lpatch?.from[hashKey],
  );

  if (hasLocalFile) return 'local';
  if (hasLpatchFile && file.lpatch) return 'hybridpatch';
  if (file.patch) return 'patch';
  return 'direct';
};

// Helper function to determine merged group mode
export const getMergedGroupMode = (
  files: DfsUpdateTask[],
  local: Embedded[],
  hashKey: DfsMetadataHashType,
): 'merged-direct' | 'merged-patch' | 'merged-direct-patch' => {
  let hasDirectFiles = false;
  let hasPatchFiles = false;

  for (const file of files) {
    const mode = getFileInstallMode(file, local, hashKey);
    if (mode === 'direct') {
      hasDirectFiles = true;
    } else if (mode === 'patch') {
      hasPatchFiles = true;
    }
    // Note: merged groups should not contain 'local' or 'hybridpatch' files
    // as they are filtered out during preprocessing
  }

  if (hasDirectFiles && hasPatchFiles) {
    return 'merged-direct-patch';
  } else if (hasPatchFiles) {
    return 'merged-patch';
  } else {
    return 'merged-direct';
  }
};

export const preprocessFiles = (
  files: DfsUpdateTask[],
  dfsSource: string,
  hashKey: DfsMetadataHashType,
  local: Embedded[],
): {
  processedFiles: (DfsUpdateTask | VirtualMergedFile)[];
  mergedGroups: Map<string, MergedGroupInfo>;
} => {
  // 按模式分离文件：只有 direct 和 patch 模式可以合并
  const mergeableFiles: DfsUpdateTask[] = [];
  const nonMergeableFiles: DfsUpdateTask[] = [];

  files.forEach((file) => {
    const mode = getFileInstallMode(file, local, hashKey);
    if (mode === 'direct' || mode === 'patch') {
      mergeableFiles.push(file);
    } else {
      // local 和 hybridpatch 文件不参与合并
      nonMergeableFiles.push(file);
    }
  });

  // 获取合并分组（只处理可合并文件）
  const groups = mergeSmallFilesIntoGroups(mergeableFiles, dfsSource, hashKey);

  // 分离单文件和合并组
  const singleFiles: DfsUpdateTask[] = [];
  const virtualMergedFiles: VirtualMergedFile[] = [];
  const mergedGroups = new Map<string, MergedGroupInfo>();

  groups.forEach((group, index) => {
    if (group.files.length === 1) {
      // 单文件直接添加到单文件列表
      singleFiles.push(group.files[0]);
    } else {
      // 多文件创建虚拟合并文件
      const virtualFileName = `__merged_group_${index}__`;
      const virtualFile: VirtualMergedFile = {
        ...group.files[0], // 继承第一个文件的基础属性
        file_name: virtualFileName,
        size: group.totalDownloadSize,
        _isMergedGroup: true,
        _mergedInfo: group,
        _fallbackFiles: [...group.files], // 保存原始文件用于fallback
        downloaded: 0,
        running: false,
        failed: undefined,
      };

      virtualMergedFiles.push(virtualFile);
      mergedGroups.set(virtualFileName, group);
    }
  });

  // 将不可合并文件按大小排序
  nonMergeableFiles.sort((a, b) => b.size - a.size); // 大文件优先

  // 按文件大小排序，实现更好的负载均衡
  singleFiles.sort((a, b) => b.size - a.size); // 大文件优先
  virtualMergedFiles.sort((a, b) => b.size - a.size); // 大合并组优先

  // 交错分配任务，实现打散效果
  const processedFiles: (DfsUpdateTask | VirtualMergedFile)[] = [];
  let singleIndex = 0;
  let mergedIndex = 0;
  let nonMergeableIndex = 0;

  // 轮流分配不同类型的文件，确保下载任务分布均匀
  while (
    singleIndex < singleFiles.length ||
    mergedIndex < virtualMergedFiles.length ||
    nonMergeableIndex < nonMergeableFiles.length
  ) {
    // 优先分配不可合并文件（local/hybridpatch）
    if (nonMergeableIndex < nonMergeableFiles.length) {
      processedFiles.push(nonMergeableFiles[nonMergeableIndex]);
      nonMergeableIndex++;
    }

    // 然后分配大文件
    if (singleIndex < singleFiles.length) {
      processedFiles.push(singleFiles[singleIndex]);
      singleIndex++;
    }

    // 最后分配合并组
    if (mergedIndex < virtualMergedFiles.length) {
      processedFiles.push(virtualMergedFiles[mergedIndex]);
      mergedIndex++;
    }
  }

  log('File preprocessing result:', {
    originalFiles: files.length,
    processedFiles: processedFiles.length,
    nonMergeableFiles: nonMergeableFiles.length, // local/hybridpatch
    mergeableSingleFiles: singleFiles.length, // direct/patch 单文件
    virtualMergedFiles: virtualMergedFiles.length, // direct/patch 合并组
    totalMergedFiles: Array.from(mergedGroups.values()).reduce(
      (sum, g) => sum + g.files.length,
      0,
    ),
    taskDistribution: processedFiles.slice(0, 12).map((f, i) => {
      const isMerged = (f as VirtualMergedFile)._isMergedGroup;
      const fileType = isMerged
        ? `合并组(${(f as VirtualMergedFile)._mergedInfo.files.length}个文件)`
        : '单文件';
      return `${i + 1}. ${fileType} - ${(f.size / 1024 / 1024).toFixed(1)}MB`;
    }),
  });

  return {
    processedFiles,
    mergedGroups,
  };
};

export const runMergedGroupDownload = async (
  groupInfo: MergedGroupInfo,
  dfsSource: string,
  extras: string | undefined,
  local: Embedded[],
  source: string,
  hashKey: DfsMetadataHashType,
  elevate: boolean,
): Promise<{ insight?: InsightItem }> => {
  // 标记组内所有文件为running
  groupInfo.files.forEach((f) => {
    f.running = true;
    f.downloaded = 0;
  });

  // Track insight for return
  let collectedInsight: InsightItem | undefined = undefined;

  try {
    // 从DFS source中提取API URL
    const { url: apiUrl, remote } = getDfsSourceType(dfsSource);
    // 获取合并后的CDN URL
    const [rangeStart, rangeEnd] = groupInfo.mergedRange.split('-').map(Number);
    let cdnUrl = apiUrl;
    if (remote === 'dfs2') {
      cdnUrl = await getDfs2Url(apiUrl, groupInfo.mergedRange);
    } else if (remote !== 'direct') {
      cdnUrl = await getDfsFileUrl(
        remote === 'plugin' ? dfsSource : apiUrl,
        extras,
        rangeEnd - rangeStart + 1,
        rangeStart,
      );
    }

    // 计算合并范围的起始位置
    const mergedRangeStart = Math.min(
      ...groupInfo.files.map((f) => (f as FileWithPosition).dfsOffset),
    );

    // 构建multichunk参数
    const chunks = groupInfo.files.map((file) => {
      const filename_with_first_slash = file.file_name.startsWith('/')
        ? file.file_name
        : `/${file.file_name}`;

      const fileWithPosition = file as FileWithPosition;
      // 计算相对于合并范围起始位置的偏移量
      const relativeOffset = fileWithPosition.dfsOffset - mergedRangeStart;

      const source_info = {
        url: cdnUrl,
        offset: relativeOffset, // 使用相对偏移而不是绝对偏移
        size: fileWithPosition.dfsSize,
        skip_decompress: false,
      };

      return {
        mode: { type: 'Direct' as const, source: source_info },
        target: source + filename_with_first_slash,
        md5: file.md5,
        xxh: file.xxh,
        type: 'InstallFile' as const,
      };
    });

    // 合并下载开始，不再记录详细的技术信息

    // 创建进度分发函数
    const progressDistributor = (event: {
      payload: { chunk_index?: number; progress?: number };
    }) => {
      const progressData = event.payload;
      if (
        progressData.chunk_index !== undefined &&
        progressData.progress !== undefined
      ) {
        const fileIndex = progressData.chunk_index;
        if (fileIndex < groupInfo.files.length) {
          groupInfo.files[fileIndex].downloaded = progressData.progress;
        }
      }
    };

    // 确定合并组模式
    const mode = getMergedGroupMode(groupInfo.files, local, hashKey);

    // 调用现有multichunk接口
    const result: {
      insight?: InsightItem;
      results?: Record<string, string>[];
    } = await ipc(
      {
        type: 'InstallMultichunkStream',
        url: cdnUrl,
        range: groupInfo.mergedRange,
        chunks: chunks,
      },
      elevate,
      progressDistributor,
    );

    // 收集合并组的网络统计
    if (result.insight) {
      addInsightWithMode(result.insight, mode);
      collectedInsight = result.insight;
    }

    // 处理结果并检查每个文件的状态
    const failedFiles: DfsUpdateTask[] = [];
    result.results?.forEach((res, index: number) => {
      if (index >= groupInfo.files.length) return;

      const file = groupInfo.files[index];
      let hasError = false;

      if (res && typeof res === 'object') {
        // 处理TAResult格式：{Ok: value} 或 {Err: error}
        if ('Err' in res) {
          hasError = true;
          // 错误信息暂存，将在后面统一处理日志
          file.errorMessage = res.Err;
        } else if ('Ok' in res) {
          // 文件成功
          file.failed = undefined;
          file.downloaded = file.size; // 标记完成
          // 成功的单个文件日志将在最后统一处理
        }
        // 兼容旧格式：直接包含error字段
        else if (res.error) {
          hasError = true;
          file.errorMessage = res.error;
        }
      } else {
        // 如果结果格式不正确，也视为错误
        hasError = true;
        file.errorMessage = `Invalid result format: ${JSON.stringify(res)}`;
      }

      if (hasError) {
        file.failed = true;
        file.downloaded = 0; // 重置下载进度
        failedFiles.push(file);
      }
    });

    // 如果有文件失败，只对失败的文件进行fallback
    if (failedFiles.length > 0) {
      // 只下载失败的文件进行fallback
      await fallbackToIndividualDownload(
        failedFiles,
        dfsSource,
        extras,
        local,
        source,
        hashKey,
        elevate,
      );
    }
    // 日志将由 MergedGroupTask 统一处理
    return { insight: collectedInsight };
  } catch (e) {
    // 处理错误情况下的网络统计
    if (e instanceof TAError && e.insight) {
      const mode = getMergedGroupMode(groupInfo.files, local, hashKey);
      addInsightWithMode(e.insight, mode);
      collectedInsight = e.insight;
    }
    throw e;
  } finally {
    // 标记组内所有文件为完成
    groupInfo.files.forEach((f) => {
      f.running = false;
      if (!f.failed) {
        f.downloaded = f.size;
      }
    });
  }
};

export const fallbackToIndividualDownload = async (
  files: DfsUpdateTask[],
  dfsSource: string,
  extras: string | undefined,
  local: Embedded[],
  source: string,
  hashKey: DfsMetadataHashType,
  elevate: boolean,
) => {
  // 重置文件状态
  files.forEach((f) => {
    f.running = false;
    f.downloaded = 0;
    f.failed = undefined;
  });

  // 逐个下载文件
  for (const file of files) {
    try {
      await runDfsDownload(
        dfsSource,
        extras,
        local,
        source,
        hashKey,
        file,
        false, // disable_patch
        false, // disable_local
        elevate,
      );
    } catch (e) {
      file.failed = true;
      throw e; // 继续向上抛出错误
    }
  }
  // Fallback 文件的日志将由对应的 SingleFileTask 处理
};
