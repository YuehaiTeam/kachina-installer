export type Lang = 'zh' | 'en';

const zh = {
  'common.error': '出错了',
  'common.notice': '提示',
  'common.cancel': '取消',
  'common.confirm': '确定',
  'common.close': '关闭',
  'common.launch': '启动',
  'common.install': '安装',
  'common.update': '更新',
  'common.uninstall': '卸载',
  'common.installTo': '安装到',
  'common.updateTo': '更新到',
  'common.uninstallFrom': '卸载自',
  'common.from': '从',

  'step1.createShortcut': '创建桌面快捷方式',
  'step1.eulaRead': '我已阅读并同意',
  'step1.eula': '用户协议',
  'step1.deleteUserData': '同时删除用户数据',
  'step1.noCdk': '无CDK',
  'step1.clickSwitchSource': '点击切换安装源',
  'step1.clickChangePath': '点击修改安装路径',

  'finish.installed': '安装完成',
  'finish.updated': '更新完成',
  'finish.alreadyLatest': '您已安装最新版本',
  'finish.uninstalling': '卸载中',
  'finish.uninstallDone': '卸载成功',

  'substep.getLatest': '获取最新版本',
  'substep.verify': '校验更新内容',
  'substep.download': '下载和解压文件',
  'substep.env': '准备运行环境',
  'substep.mirrorcGetLatest': '从 Mirror酱 获取最新版本',
  'substep.mirrorcDownload': '下载数据包',
  'substep.mirrorcExtract': '解压文件',

  'status.verifyLocal': '校验本地文件……',
  'status.creatingSession': '创建下载会话……',
  'status.prepareDownload': '准备下载……',
  'status.deletingOld': '删除旧版残留文件……',
  'status.almostDone': '很快就好……',
  'status.runtimeInstalling': '安装运行库……',
  'status.runtimeInstallName': '安装{name}……',
  'status.runtimeDownloadName': '下载 {name} ……',
  'status.mirrorcPreparing': '准备从Mirror酱下载……',
  'status.checkingZip': '检查压缩包……',
  'status.extracting': '解压 {file}',
  'status.deleting': '删除 {file}',

  'confirm.appRunning': '检测到{appName}正在运行，是否结束进程并继续安装？',
  'confirm.notLatest': '当前安装包不是最新版本，是否直接安装最新版本？',
  'confirm.filesInUse':
    '检测到部分文件被占用，继续安装可能无法成功，是否继续？\n\n被占用的文件列表：',
  'confirm.dirNotEmpty':
    '您选择的目录不为空，是否创建新文件夹再安装？选【否】将可能影响原有数据。',

  'err.getMetaFailed': '获取更新信息失败，请检查网络连接',
  'err.unknownCheckLog': '：未知错误，请检查日志',
  'err.unsupportedHash': '更新服务端配置有误，不支持的哈希算法',
  'err.createSessionFailed': '创建下载会话失败: {err}',
  'err.killFailed': '结束进程失败: {err}',
  'err.runtimeInstallFailed': '安装{name}失败: {err}，请手动安装',
  'err.createUninstallerFailed': '创建卸载程序失败: {err}',
  'err.writeRegistryFailed': '写入注册表失败: {err}',
  'err.noNode': '没有可用的下载节点：',
  'err.mirrorcBadSource': '无法获取Mirror酱数据，安装包可能已经损坏：{url}',
  'err.mirrorcFetchFailed': '从Mirror酱获取更新数据失败: {err}',
  'err.mirrorcNoUrl':
    '从Mirror酱获取更新失败: 下载地址为空，请联系Mirror酱客服',
  'err.mirrorcNoSha':
    '从Mirror酱获取更新失败: 校验数据为空，请联系Mirror酱客服',
  'err.packConfigMissing': '打包错误，请确保配置文件被正确打包',
  'err.configNotFoundDev': '未找到配置文件，请将配置文件放在exe同目录下',
  'err.packBroken': '安装包损坏，请重新下载',
  'err.packIndexWrong': '打包错误，请确保索引文件正确',
  'err.uninstallMetaMissing': '未找到卸载配置文件，请重新安装后再卸载',
  'err.initFailed': '安装程序初始化失败',

  'net.timeout': '连接下载服务器超时，请检查你的网络连接或更换下载源',
  'net.refused': '下载服务器出现问题，请重试或更换下载源',
  'net.reset': '连接下载服务器失败，请重试或更换下载源',
  'net.slow': '检测到下载速度异常，请检查你的网络连接或更换下载源',
  'net.originalError': '原始错误：',
  'net.server': '下载服务器：',

  'mirrorcErr.1001': 'Mirror酱参数错误，请检查打包配置',
  'mirrorcErr.7001': 'Mirror酱 CDK 已过期',
  'mirrorcErr.7002': 'Mirror酱 CDK 错误，请检查设置的 CDK 是否正确',
  'mirrorcErr.7003': 'Mirror酱 CDK 今日下载次数已达上限，请更换 CDK 或明天再试',
  'mirrorcErr.7004':
    'Mirror酱 CDK 类型和待下载的资源不匹配，请检查设置的 CDK 是否正确',
  'mirrorcErr.7005': 'Mirror酱 CDK 已被封禁，请更换 CDK',
  'mirrorcErr.8001': '从Mirror酱获取更新失败，请检查打包配置',
  'mirrorcErr.unknownMsg': '未知错误',
  'mirrorcErr.unknownCdk':
    '从Mirror酱获取CDK状态失败: {msg}，请联系Mirror酱客服',
  'mirrorcErr.unknown': '从Mirror酱获取更新失败: {msg}，请联系Mirror酱客服',

  'dialog.selectSource': '选择安装源',
  'dialog.selectSourceDesc': '{title}支持多种在线安装方式。',
  'dialog.mirrorcTitle': '设置 Mirror酱 CDK',
  'dialog.mirrorcDesc':
    'Mirror酱是独立的第三方软件下载平台，提供付费的软件下载加速服务。<br />如果你有 Mirror酱的 CDK，可以在这里输入。',
  'dialog.mirrorcPlaceholder': '请输入 Mirror酱 CDK',
  'dialog.getCdk': '获取 CDK',

  'lnk.uninstallName': '卸载{appName}',
  'input.placeholder': '请输入内容',
  'err.releaseFileFailed': '释放文件 {name} 失败：\n{err}',
} as const;

export type I18nKey = keyof typeof zh;

const en: Record<I18nKey, string> = {
  'common.error': 'Error',
  'common.notice': 'Notice',
  'common.cancel': 'Cancel',
  'common.confirm': 'OK',
  'common.close': 'Close',
  'common.launch': 'Launch',
  'common.install': 'Install',
  'common.update': 'Update',
  'common.uninstall': 'Uninstall',
  'common.installTo': 'Install to',
  'common.updateTo': 'Update to',
  'common.uninstallFrom': 'Uninstall from',
  'common.from': 'From',

  'step1.createShortcut': 'Create desktop shortcut',
  'step1.eulaRead': 'I have read and agree to the',
  'step1.eula': 'User Agreement',
  'step1.deleteUserData': 'Also delete user data',
  'step1.noCdk': 'No CDK',
  'step1.clickSwitchSource': 'Click to change download source',
  'step1.clickChangePath': 'Click to change install path',

  'finish.installed': 'Installation complete',
  'finish.updated': 'Update complete',
  'finish.alreadyLatest': 'You already have the latest version',
  'finish.uninstalling': 'Uninstalling…',
  'finish.uninstallDone': 'Uninstall complete',

  'substep.getLatest': 'Checking for the latest version',
  'substep.verify': 'Verifying update contents',
  'substep.download': 'Downloading and extracting files',
  'substep.env': 'Preparing runtime environment',
  'substep.mirrorcGetLatest': 'Checking MirrorChyan for the latest version',
  'substep.mirrorcDownload': 'Downloading package',
  'substep.mirrorcExtract': 'Extracting files',

  'status.verifyLocal': 'Verifying local files…',
  'status.creatingSession': 'Creating download session…',
  'status.prepareDownload': 'Preparing to download…',
  'status.deletingOld': 'Removing leftover files from the old version…',
  'status.almostDone': 'Almost done…',
  'status.runtimeInstalling': 'Installing runtimes…',
  'status.runtimeInstallName': 'Installing {name}…',
  'status.runtimeDownloadName': 'Downloading {name}…',
  'status.mirrorcPreparing': 'Preparing to download from MirrorChyan…',
  'status.checkingZip': 'Verifying archive…',
  'status.extracting': 'Extracting {file}',
  'status.deleting': 'Deleting {file}',

  'confirm.appRunning':
    '{appName} is running. Close it and continue with the installation?',
  'confirm.notLatest':
    'This package is not the latest version. Install the latest version directly?',
  'confirm.filesInUse':
    'Some files are in use, installation may fail if you continue. Continue anyway?\n\nLocked files:',
  'confirm.dirNotEmpty':
    'The selected folder is not empty. Create a new folder for the installation? Choosing "No" may affect existing data.',

  'err.getMetaFailed':
    'Failed to fetch update information. Please check your network connection',
  'err.unknownCheckLog': ': unknown error, please check the log',
  'err.unsupportedHash':
    'The update server is misconfigured: unsupported hash algorithm',
  'err.createSessionFailed': 'Failed to create download session: {err}',
  'err.killFailed': 'Failed to close the process: {err}',
  'err.runtimeInstallFailed':
    'Failed to install {name}: {err}. Please install it manually',
  'err.createUninstallerFailed': 'Failed to create the uninstaller: {err}',
  'err.writeRegistryFailed': 'Failed to write to the registry: {err}',
  'err.noNode': 'No available download nodes: ',
  'err.mirrorcBadSource':
    'Cannot fetch MirrorChyan data, the package may be corrupted: {url}',
  'err.mirrorcFetchFailed':
    'Failed to fetch update data from MirrorChyan: {err}',
  'err.mirrorcNoUrl':
    'Failed to fetch update from MirrorChyan: the download URL is empty. Please contact MirrorChyan support',
  'err.mirrorcNoSha':
    'Failed to fetch update from MirrorChyan: the checksum is empty. Please contact MirrorChyan support',
  'err.packConfigMissing':
    'Packaging error: please make sure the config file is packed correctly',
  'err.configNotFoundDev':
    'Config file not found. Please place it next to the exe',
  'err.packBroken':
    'The installer package is corrupted. Please download it again',
  'err.packIndexWrong':
    'Packaging error: please make sure the index file is correct',
  'err.uninstallMetaMissing':
    'Uninstall metadata not found. Please reinstall before uninstalling',
  'err.initFailed': 'Installer initialization failed',

  'net.timeout':
    'Connection to the download server timed out. Please check your network or switch to another download source',
  'net.refused':
    'The download server is having problems. Please retry or switch to another download source',
  'net.reset':
    'Failed to connect to the download server. Please retry or switch to another download source',
  'net.slow':
    'Abnormally slow download detected. Please check your network or switch to another download source',
  'net.originalError': 'Original error: ',
  'net.server': 'Download server: ',

  'mirrorcErr.1001':
    'MirrorChyan parameter error. Please check the packaging configuration',
  'mirrorcErr.7001': 'The MirrorChyan CDK has expired',
  'mirrorcErr.7002':
    'Invalid MirrorChyan CDK. Please check that the CDK is correct',
  'mirrorcErr.7003':
    'The MirrorChyan CDK has reached its daily download limit. Please use another CDK or try again tomorrow',
  'mirrorcErr.7004':
    'The MirrorChyan CDK type does not match this resource. Please check that the CDK is correct',
  'mirrorcErr.7005':
    'The MirrorChyan CDK has been banned. Please use another CDK',
  'mirrorcErr.8001':
    'Failed to fetch update from MirrorChyan. Please check the packaging configuration',
  'mirrorcErr.unknownMsg': 'unknown error',
  'mirrorcErr.unknownCdk':
    'Failed to fetch CDK status from MirrorChyan: {msg}. Please contact MirrorChyan support',
  'mirrorcErr.unknown':
    'Failed to fetch update from MirrorChyan: {msg}. Please contact MirrorChyan support',

  'dialog.selectSource': 'Select download source',
  'dialog.selectSourceDesc':
    '{title} supports multiple online installation methods.',
  'dialog.mirrorcTitle': 'Set MirrorChyan CDK',
  'dialog.mirrorcDesc':
    'MirrorChyan is an independent third-party software download platform offering paid download acceleration.<br />If you have a MirrorChyan CDK, you can enter it here.',
  'dialog.mirrorcPlaceholder': 'Enter your MirrorChyan CDK',
  'dialog.getCdk': 'Get a CDK',

  'lnk.uninstallName': 'Uninstall {appName}',
  'input.placeholder': 'Enter text',
  'err.releaseFileFailed': 'Failed to release {name}:\n{err}',
};

const messages: Record<Lang, Record<I18nKey, string>> = { zh, en };

function detectLang(): Lang {
  const langs = [navigator.language, ...(navigator.languages || [])];
  return langs.some((l) => /^zh/i.test(l || '')) ? 'zh' : 'en';
}

let currentLang: Lang = detectLang();

export function setLanguage(pref: 'auto' | 'zh' | 'en' = 'auto') {
  currentLang = pref === 'auto' ? detectLang() : pref;
}

export function getLanguage(): Lang {
  return currentLang;
}

export function tFor(
  lang: Lang,
  key: I18nKey,
  params?: Record<string, string | number>,
): string {
  let str = messages[lang][key];
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      str = str.split(`{${k}}`).join(String(v));
    }
  }
  return str;
}

export function t(
  key: I18nKey,
  params?: Record<string, string | number>,
): string {
  let str = messages[currentLang][key];
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      str = str.split(`{${k}}`).join(String(v));
    }
  }
  return str;
}
