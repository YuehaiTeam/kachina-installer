import type { Dfs2Data, InvokeGetDfsMetadataRes, Embedded } from '../types';

export interface KachinaInstallSource {
  name: string;
  matchUrl: (url: string) => boolean;
  
  // 可选：自定义元数据获取，返回完整的DFS2数据结构
  getMetadata?: (url: string) => Promise<Dfs2Data>;
  
  // 可选：会话管理（插件自己管理sessionId）
  createSession?: (url: string, diffchunks: string[]) => Promise<string>;
  endSession?: (url: string, insights: any) => Promise<void>;
  
  // 必需：获取文件块URL
  getChunkUrl: (url: string, range: string) => Promise<{url: string, range: string}>;
}