type ProjectConfig = {
  dfsPath: string;
  appName: string;
  publisher: string;
  regName: string;
  exeName: string;
  uninstallName: string;
  updaterName: string;
  programFilesPath: string;
  title: string;
  description: string;
  windowTitle: string;
  foldedInstallDir: bool;
};

type InstallStat = {
  downloadedTotalSize: number;
  speedLastSize: number;
  lastTime: DOMHighResTimeStamp;
  speed: number;
  runningTasks: Record<string, string>;
};

type InvokeGetInstallSourceRes = [string, boolean];

type DfsMetadataHashType = 'md5' | 'xxh';

type DfsMetadataHashInfo = {
  file_name: string;
  size: number;
  md5?: string;
  xxh?: string;
};

type DfsMetadataPatchInfo = {
  file_name: string;
  size: number;
  from: Omit<DfsMetadataHashInfo, 'file_name'>;
  to: Omit<DfsMetadataHashInfo, 'file_name'>;
};

interface DfsUpdateTask extends DfsMetadataHashInfo {
  patch?: DfsMetadataPatchInfo;
}

type InvokeGetDfsMetadataRes = {
  tag_name: string;
  hashed: Array<DfsMetadataHashInfo>;
  patches?: Array<DfsMetadataPatchInfo>;
};

type InvokeDeepReaddirWithMetadataRes = Array<{
  file_name: string;
  size: number;
  hash: string;
}>;

type InvokeGetDfsRes = {
  url?: string;
  tests?: Array<[string, string]>;
  source: string;
};

type InvokeGetDirsRes = [string, string];

type InvokeSelectDirRes = string | null;
