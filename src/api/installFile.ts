type InstallFileSource = string | { offset: number; size: number };

type InstallFileMode =
  | { type: 'Direct'; source: InstallFileSource }
  | { type: 'Patch'; source: InstallFileSource; diff_size: number }
  | {
      type: 'HybridPatch';
      diff_url: string;
      diff_size: number;
      source_offset: number;
      source_size: number;
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
  source: InstallFileSource,
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
  return { mode, target, type: 'InstallFile', ...hash };
}

/**
 * @param diff_url - 差异文件的 URL
 * @param diff_size - 差异文件的大小
 * @param source_offset - 源文件的偏移量
 * @param source_size - 源文件的大小
 * @param target - 目标路径
 */
export function hybridPatch(
  source: { offset: number; size: number },
  diff_url: string,
  diff_size: number,
  target: string,
  hash: {
    xxh?: string;
    md5?: string;
  },
): InstallFileArgs {
  const mode: InstallFileMode = {
    type: 'HybridPatch',
    diff_url,
    diff_size,
    source_offset: source.offset,
    source_size: source.size,
  };

  return { mode, target, type: 'InstallFile', ...hash };
}
