// ^(?:(dfs)\+)?(?:(hashed|packed|auto)\+)?(http(?:s)?:\/\/(?:.*?))$
interface SourceItem {
  uri: string;
  id: string;
  name: string;
  hidden: boolean;
}
type ProjectConfig = {
  source: string | SourceItem[];
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
  // UAC 策略
  // prefer-admin: 除非用户安装在%User%、%AppData%、%Documents%、%Desktop%、%Downloads%目录，都请求UAC
  // prefer-user: 只在用户没有权限写入的目录请求UAC
  // force: 强制请求UAC
  uacStrategy: 'prefer-admin' | 'prefer-user' | 'force';
  runtimes?: string[];
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
  installer?: true;
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
  old_hash?: string;
  unwritable: boolean;
  failed?: true;
}

type InvokeGetDfsMetadataRes = {
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

type InvokeDeepReaddirWithMetadataRes = Array<{
  file_name: string;
  size: number;
  hash: string;
  unwritable: boolean;
}>;

type InvokeGetDfsRes = {
  url?: string;
  tests?: Array<[string, string]>;
  source: string;
};

type InvokeGetDirsRes = [string, string];

type InvokeSelectDirRes = {
  path: string;
  state: 'Unwritable' | 'Writable' | 'Private';
  empty: boolean;
  upgrade: boolean;
} | null;

interface Embedded {
  name: string;
  offset: number;
  raw_offset: number;
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
  embedded_index: Embedded[] | null;
  embedded_config: ProjectConfig | null;
  enbedded_metadata: InvokeGetDfsMetadataRes | null;
  exe_path: string;
  args: {
    target: string | null;
    non_interactive: boolean;
    silent: boolean;
    online: boolean;
    uninstall: boolean;
    source?: string;
    dfs_extras?: string;
    mirrorc_cdk?: string;
  };
  elevated: boolean;
}
