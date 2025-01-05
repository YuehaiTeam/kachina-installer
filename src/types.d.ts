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

type InvokeGetDfsMetadataRes = {
  tag_name: string;
  hashed: Array<DfsMetadataHashInfo>;
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
