import { error } from './api/ipc';
import { t, type I18nKey } from './i18n';

/**
 * Mirror酱错误码对应表
 */
export interface MirrorcErrorInfo {
  code: number;
  message: string;
  showSourceDialog?: boolean;
}

export const MIRRORC_ERROR_CODES: Record<
  number,
  { key: I18nKey; showSourceDialog?: boolean }
> = {
  1001: {
    key: 'mirrorcErr.1001',
  },
  7001: {
    key: 'mirrorcErr.7001',
    showSourceDialog: true,
  },
  7002: {
    key: 'mirrorcErr.7002',
    showSourceDialog: true,
  },
  7003: {
    key: 'mirrorcErr.7003',
  },
  7004: {
    key: 'mirrorcErr.7004',
    showSourceDialog: true,
  },
  7005: {
    key: 'mirrorcErr.7005',
    showSourceDialog: true,
  },
  8001: {
    key: 'mirrorcErr.8001',
  },
  8002: {
    key: 'mirrorcErr.1001',
  },
  8003: {
    key: 'mirrorcErr.1001',
  },
  8004: {
    key: 'mirrorcErr.1001',
  },
};

/**
 * 获取Mirror酱错误信息
 * @param code 错误码
 * @returns 错误信息，如果不是已知错误码则返回null
 */
export function getMirrorcErrorInfo(code: number): MirrorcErrorInfo | null {
  const info = MIRRORC_ERROR_CODES[code];
  if (!info) return null;
  return {
    code,
    message: t(info.key),
    showSourceDialog: info.showSourceDialog,
  };
}

/**
 * 处理Mirror酱错误并记录日志
 * @param mirrorcStatus Mirror酱状态响应
 * @param contextType 错误上下文类型（用于日志区分）
 * @returns 处理后的错误信息
 */
export function processMirrorcError(
  mirrorcStatus: { code: number; msg?: string },
  contextType: 'install' | 'cdk-validation' = 'install',
): {
  isError: boolean;
  errorInfo: MirrorcErrorInfo;
  message: string;
  showSourceDialog: boolean;
} | null {
  if (mirrorcStatus.code === 0) {
    return null;
  }

  const errorInfo = getMirrorcErrorInfo(mirrorcStatus.code);

  if (errorInfo) {
    // 记录已知错误码（日志不跟随界面语言，只记错误码和原始 msg）
    error(
      `Mirror酱${contextType === 'cdk-validation' ? 'CDK验证' : ''}错误 [${mirrorcStatus.code}]: ${mirrorcStatus.msg || '无详细信息'}`,
    );

    return {
      isError: true,
      errorInfo,
      message: errorInfo.message,
      showSourceDialog: errorInfo.showSourceDialog || false,
    };
  } else {
    // 处理未知错误码
    const unknownMessage = t(
      contextType === 'cdk-validation'
        ? 'mirrorcErr.unknownCdk'
        : 'mirrorcErr.unknown',
      { msg: mirrorcStatus.msg || t('mirrorcErr.unknownMsg') },
    );

    // 记录未知错误码
    error(
      `Mirror酱${contextType === 'cdk-validation' ? 'CDK验证' : ''}未知错误 [${mirrorcStatus.code}]: ${mirrorcStatus.msg || '无详细信息'}`,
    );

    return {
      isError: true,
      errorInfo: {
        code: mirrorcStatus.code,
        message: unknownMessage,
      },
      message: unknownMessage,
      showSourceDialog: false,
    };
  }
}
