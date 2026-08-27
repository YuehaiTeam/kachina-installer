import { getCurrentWindow, invoke } from './tauri';
import { error, log, sendInsight, warn } from './api/ipc';
import { dialogError, insightBase, uacNeeded } from './ui';
import { getLanguage, setLanguage, t } from './i18n';
import type {
  InstallerConfig,
  InvokeSelectDirRes,
  ProjectConfig,
} from './types';

export type EmbeddedTheme = {
  imageSource: string;
  dynamicCss: string;
  useDynamicCss: boolean;
};

export type BootstrapResult = {
  selectedSource: string;
  source: string;
  isUpdate: boolean;
  needElevate: boolean;
  theme: EmbeddedTheme;
  autoRun: 'install' | 'uninstall' | null;
  createLnk: boolean;
  deleteUserData: boolean;
  mirrorcKey: string;
};

const defaultImage = () => new URL('./left.webp', import.meta.url).href;

export function processEmbeddedImage(base64Data: string | null): EmbeddedTheme {
  if (!base64Data) {
    return {
      imageSource: defaultImage(),
      dynamicCss: '',
      useDynamicCss: false,
    };
  }

  try {
    const binaryString = atob(base64Data);
    const bytes = new Uint8Array(binaryString.length);
    for (let i = 0; i < binaryString.length; i++) {
      bytes[i] = binaryString.charCodeAt(i);
    }

    const first16Bytes = bytes.slice(0, Math.min(16, bytes.length));
    const isAscii = first16Bytes.every((byte) => byte >= 0x20 && byte <= 0x7e);

    if (isAscii) {
      log('Loaded embedded CSS stylesheet');
      return {
        imageSource: '',
        dynamicCss: new TextDecoder().decode(bytes),
        useDynamicCss: true,
      };
    }
    log('Loaded embedded image');
    return {
      imageSource: `data:image/webp;base64,${base64Data}`,
      dynamicCss: '',
      useDynamicCss: false,
    };
  } catch (e) {
    error('Failed to process embedded image:', e);
    return {
      imageSource: defaultImage(),
      dynamicCss: '',
      useDynamicCss: false,
    };
  }
}

async function getInstallerConfig(scan: boolean): Promise<InstallerConfig> {
  return await invoke<InstallerConfig>('get_installer_config', {
    scanExe: scan,
  });
}

export async function bootstrap(
  project: ProjectConfig,
  installer: InstallerConfig,
): Promise<BootstrapResult | null> {
  const win = getCurrentWindow();
  const showTasks: Promise<unknown>[] = [win.setTitle(' ')];
  if (process.env.NODE_ENV === 'development') {
    showTasks.push(win.show());
  }

  let rsrc = await getInstallerConfig(false);
  Object.assign(installer, rsrc);
  if (!rsrc.args.silent) {
    await win.show();
  }
  await Promise.all(showTasks);

  rsrc = await getInstallerConfig(true);
  Object.assign(installer, rsrc);
  log('INSTALLER_CONFIG: ', {
    ...rsrc,
    embedded_config: {
      ...rsrc.embedded_config,
      source: Array.isArray(rsrc.embedded_config?.source)
        ? rsrc.embedded_config?.source.map((e) => ({ id: e.id, uri: e.uri }))
        : rsrc.embedded_config?.source,
    },
    embedded_index: undefined,
    embedded_files: undefined,
    embedded_image: undefined,
    enbedded_metadata: undefined,
  });

  if (installer.embedded_config) {
    Object.assign(project, installer.embedded_config);
    setLanguage(project.language ?? 'auto');
    await invoke('set_language', { language: getLanguage() }).catch(log);
    if (
      process.env.NODE_ENV === 'development' &&
      installer.embedded_files &&
      installer.embedded_files.length > 0 &&
      !installer.embedded_files.find((e) => e.name === '\0CONFIG')
    ) {
      dialogError(
        t('err.packConfigMissing'),
        t('common.error'),
        installer.args.silent,
      );
    }
  } else if (process.env.NODE_ENV === 'development') {
    dialogError(
      t('err.configNotFoundDev'),
      t('common.error'),
      installer.args.silent,
    );
  } else {
    await dialogError(
      t('err.packBroken'),
      t('common.error'),
      installer.args.silent,
    );
    win.close();
    return null;
  }

  const xsrc = rsrc.embedded_config?.source;
  if (!xsrc) {
    throw new Error(t('err.packConfigMissing'));
  }

  let selectedSource = '';
  if (installer.preset?.source_uri) {
    selectedSource = installer.preset.source_uri;
  } else if (!Array.isArray(xsrc)) {
    selectedSource = xsrc;
  } else if (xsrc.length > 0) {
    selectedSource =
      xsrc.find((e) => e.id === rsrc.args.source)?.uri || xsrc[0]?.uri;
  }

  const source =
    installer.preset?.install_path ||
    installer.args.target ||
    installer.install_path;
  let needElevate = true;
  const seldir = await invoke<InvokeSelectDirRes>('select_dir', {
    exeName: project.exeName,
    silent: true,
    path: source,
  });
  if (seldir) {
    needElevate = uacNeeded(seldir.state, project.uacStrategy);
  }

  if (installer.embedded_index && installer.embedded_files) {
    let hasWrongIndex = false;
    for (const i of installer.embedded_index) {
      const target = installer.embedded_files.find((e) => e.name === i.name);
      if (!target) {
        log('Unfound index', target, i);
        hasWrongIndex = true;
        continue;
      }
      if (target.offset !== i.offset || target.raw_offset !== i.raw_offset) {
        log('Wrong index: pack=', target, 'index=', i);
        hasWrongIndex = true;
      }
    }
    if (hasWrongIndex) {
      if (process.env.NODE_ENV === 'development') {
        dialogError(
          t('err.packIndexWrong'),
          t('common.error'),
          installer.args.silent,
        );
      } else {
        await dialogError(
          t('err.packBroken'),
          t('common.error'),
          installer.args.silent,
        );
        win.close();
        return null;
      }
    }
  }

  sendInsight(insightBase(installer, project));
  const isUpdate = installer.install_path_exists;
  await win.setTitle(project.windowTitle);
  installer.is_uninstall = installer.is_uninstall || installer.args.uninstall;

  if (installer.is_uninstall) {
    const uninstallConfig = await invoke(
      'read_uninstall_metadata',
      project,
    ).catch(log);
    log('UNINSTALL_METADATA: ', uninstallConfig);
    if (!uninstallConfig) {
      await dialogError(
        t('err.uninstallMetaMissing'),
        t('common.error'),
        installer.args.silent,
      );
      if (process.env.NODE_ENV !== 'development') {
        win.close();
      }
      return null;
    }
  }

  if (project.windowBorderless === true) {
    try {
      await win.setDecorations(false);
    } catch (e) {
      warn('Failed to set window borderless:', e);
    }
  } else {
    try {
      await win.setDecorations(true);
    } catch (e) {
      warn('Failed to set window decorations:', e);
    }
  }

  let autoRun: BootstrapResult['autoRun'] = null;
  if (installer.args.silent || installer.args.non_interactive) {
    autoRun =
      installer.args.uninstall || installer.is_uninstall
        ? 'uninstall'
        : 'install';
  }

  return {
    selectedSource,
    source,
    isUpdate,
    needElevate,
    theme: processEmbeddedImage(installer.embedded_image),
    autoRun,
    createLnk: installer.preset?.create_lnk ?? true,
    deleteUserData: installer.preset?.delete_user_data ?? false,
    mirrorcKey: installer.preset?.mirrorc_cdk ?? '',
  };
}
