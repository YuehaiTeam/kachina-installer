type InstallFileSource =
  | { url: string; offset: number; size: number; skip_decompress?: boolean }
  | { offset: number; size: number; skip_decompress?: boolean };

type InstallFileMode =
  | { type: 'Direct'; source: InstallFileSource }
  | { type: 'Patch'; source: InstallFileSource; diff_size: number }
  | {
      type: 'HybridPatch';
      diff: InstallFileSource;
      source: InstallFileSource;
    };

interface InstallFileArgs {
  mode: InstallFileMode;
  target: string;
  xxh?: string;
  md5?: string;
  type: 'InstallFile';
}

/**
 * @param source - 文件来源（Url 字符串或 Local 对象）
 * @param target - 目标路径
 * @param diff_size - Patch 模式需要的 diff_size
 */
export function InstallFile(
  source: InstallFileSource & { skip_hash?: boolean },
  target: string,
  hash: {
    xxh?: string;
    md5?: string;
  },
  diff_size?: number,
): InstallFileArgs {
  let mode: InstallFileMode;
  if (!diff_size) {
    mode = { type: 'Direct', source };
  } else {
    mode = { type: 'Patch', source, diff_size };
  }
  if (source.skip_hash) {
    delete hash.xxh;
    delete hash.md5;
  }
  return { mode, target, type: 'InstallFile', ...hash };
}

export function hybridPatch(
  source: { offset: number; size: number },
  diff: { url: string; offset: number; size: number },
  target: string,
  hash: {
    xxh?: string;
    md5?: string;
  },
): InstallFileArgs {
  const mode: InstallFileMode = {
    type: 'HybridPatch',
    diff,
    source,
  };

  return { mode, target, type: 'InstallFile', ...hash };
}
