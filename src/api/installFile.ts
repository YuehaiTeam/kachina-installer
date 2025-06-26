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

interface InstallMultipartStreamArgs {
  url: string;
  range: string;
  chunks: InstallFileArgs[];
  type: 'InstallMultipartStream';
}

interface InstallMultichunkStreamArgs {
  url: string;
  range: string;
  chunks: InstallFileArgs[];
  type: 'InstallMultichunkStream';
}

export function getRangeParams(chunks: InstallFileArgs[]): {
  total_start: number;
  total_end: number;
  multipart: string;
  ranges: [number, number][];
} {
  let total_start = Infinity;
  let total_end = -1;
  const ranges: [number, number][] = [];
  const multipart: string[] = [];

  for (const chunk of chunks) {
    const { offset, size } = chunk.mode.source;
    if (offset < total_start) {
      total_start = offset;
    }
    if (offset + size > total_end) {
      total_end = offset + size;
    }
    ranges.push([offset, offset + size - 1]);
    multipart.push(`${offset}-${offset + size - 1}`);
  }

  return {
    total_start,
    total_end,
    multipart: multipart.join(','),
    ranges,
  };
}

/**
 * 安装多部分流文件 - 用于处理服务器支持 multipart/byteranges 的情况
 * @param url - 文件 URL
 * @param range - HTTP Range 范围，如 "100-200,300-400,500-600"
 * @param chunks - 要安装的文件块列表
 */
export function InstallMultipartStream(
  url: string,
  range: string,
  chunks: InstallFileArgs[],
): InstallMultipartStreamArgs {
  return {
    url,
    range,
    chunks,
    type: 'InstallMultipartStream',
  };
}

/**
 * 安装多块流文件 - 用于处理非连续块的单一 HTTP Range 请求
 * @param url - 文件 URL
 * @param range - 总的 HTTP Range 范围，如 "0-1024"
 * @param chunks - 要安装的文件块列表，每个块的 offset 字段指定在流中的位置
 */
export function InstallMultichunkStream(
  url: string,
  range: string,
  chunks: InstallFileArgs[],
): InstallMultichunkStreamArgs {
  return {
    url,
    range,
    chunks,
    type: 'InstallMultichunkStream',
  };
}

export async function createMultiInstall(
  chunks: InstallFileArgs[],
  multipart = true,
  getUrl: (ranges: ReturnType<typeof getRangeParams>) => Promise<string>,
) {
  const ranges = getRangeParams(chunks);
  if (multipart) {
    return InstallMultipartStream(
      await getUrl(ranges),
      ranges.multipart,
      chunks,
    );
  } else {
    return InstallMultichunkStream(
      await getUrl(ranges),
      `${ranges.total_start}-${ranges.total_end}`,
      chunks,
    );
  }
}
