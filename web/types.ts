export type InstallStat = {
  speedLastSize: number;
  lastTime: DOMHighResTimeStamp;
  speed: number;
};

export type DfsMetadataHashType = 'md5' | 'xxh';

export type DfsMetadataHashInfo = {
  file_name: string;
  size: number;
  md5?: string;
  xxh?: string;
  installer?: true;
};

export type DfsMetadataPatchInfo = {
  file_name: string;
  size: number;
  from: Omit<DfsMetadataHashInfo, 'file_name'>;
  to: Omit<DfsMetadataHashInfo, 'file_name'>;
};

export interface DfsUpdateTask extends DfsMetadataHashInfo {
  patch?: DfsMetadataPatchInfo;
  lpatch?: DfsMetadataPatchInfo;
  downloaded: number;
  running: boolean;
  old_hash?: string;
  unwritable: boolean;
  failed?: true;
  errorMessage?: string; // 用于存储合并下载中的单个文件错误信息
}

// 合并下载相关类型定义
export interface FileWithPosition extends DfsUpdateTask {
  dfsOffset: number;
  dfsSize: number;
}

export interface MergedGroupInfo {
  files: DfsUpdateTask[];
  mergedRange: string;
  totalDownloadSize: number;
  totalEffectiveSize: number;
  wasteRatio: number;
  gaps: Array<{ start: number; end: number }>;
}

export interface VirtualMergedFile extends DfsUpdateTask {
  _isMergedGroup: true;
  _mergedInfo: MergedGroupInfo;
  _fallbackFiles: DfsUpdateTask[];
}

export type InvokeGetDfsMetadataRes = {
  tag_name: string;
  hashed: Array<DfsMetadataHashInfo>;
  patches?: Array<DfsMetadataPatchInfo>;
  installer?: {
    size: number;
    md5?: string;
    xxh?: string;
  };
  deletes?: string[];
};

export type InvokeDeepReaddirWithMetadataRes = Array<{
  file_name: string;
  size: number;
  hash: string;
  unwritable: boolean;
}>;

export type InvokeGetDfsRes = {
  url?: string;
  tests?: Array<[string, string]>;
  source: string;
};

// DFS2 types
export type Dfs2Metadata = {
  resource_version: string;
  name: string;
  data: Dfs2Data | null;
};

export type Dfs2Data = {
  index: Record<string, Dfs2FileInfo>;
  metadata: InvokeGetDfsMetadataRes;
  installer_end: number;
};

export type Dfs2FileInfo = {
  name: string;
  offset: number;
  raw_offset: number;
  size: number;
};

export type Dfs2SessionResponse = {
  tries?: string[];
  sid?: string;
  challenge?: string;
  data?: string;
};

export type Dfs2ChunkResponse = {
  url: string;
};

export type Dfs2BatchChunkRequest = {
  chunks: string[];
};

export type Dfs2ChunkUrlResult = {
  url?: string;
  error?: string;
};

export type Dfs2BatchChunkResponse = {
  urls: Record<string, Dfs2ChunkUrlResult>;
};

export interface InsightItem {
  url: string;
  ttfb: number; // 首字节时间(ms)
  time: number; // 纯下载时间(ms) = 总时间 - TTFB
  size: number; // 实际下载字节数
  error?: string;
  range?: [number, number][]; // HTTP Range请求范围
  mode?: string; // 安装模式
}

export interface InstallResult {
  bytes_transferred: number;
  insight?: InsightItem;
}

export type Dfs2SessionInsights = {
  servers: InsightItem[];
};

export interface TAErrorData {
  message: string;
  insight?: InsightItem;
}

export class TAError extends Error {
  public readonly insight?: InsightItem;

  constructor(data: TAErrorData | string) {
    if (typeof data === 'string') {
      super(data);
    } else {
      super(data.message);
      this.insight = data.insight;
    }
  }

  static fromErrorData(data: TAErrorData): TAError {
    return new TAError(data);
  }
}

export type InvokeGetDirsRes = [string, string];

export type InvokeSelectDirRes = {
  path: string;
  state: 'Unwritable' | 'Writable' | 'Private';
  empty: boolean;
  upgrade: boolean;
} | null;
