type ProjectConfig = {
  dfsPath: string;
  appName: string;
  publisher: string;
  regName: string;
  exeName: string;
  uninstallName: string;
  updaterName: string;
  programFilesPath: string;
  userDataPath: string[];
  extraUninstallPath: string[];
  title: string;
  description: string;
  windowTitle: string;
};

type InstallStat = {
  speedLastSize: number;
  lastTime: DOMHighResTimeStamp;
  speed: number;
};

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
  lpatch?: DfsMetadataPatchInfo;
  downloaded: number;
  running: boolean;
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

interface Embedded {
  name: String;
  offset: number;
  size: number;
}

interface InstallerConfig {
  install_path: string;
  install_path_exists: boolean;
  install_path_source:
    | 'CURRENT_DIR'
    | 'PARENT_DIR'
    | 'REG'
    | 'REG_FOLDED'
    | 'DEFAULT';
  is_uninstall: boolean;
  embedded_files: Embedded[] | null;
  embedded_config: ProjectConfig | null;
  enbedded_metadata: InvokeGetDfsMetadataRes | null;
  exe_path: string;
  args: {
    target: string | null;
    non_interactive: boolean;
    silent: boolean;
    online: boolean;
    uninstall: boolean;
  };
}
