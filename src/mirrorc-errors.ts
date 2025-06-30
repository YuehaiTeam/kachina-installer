import { error } from './api/ipc';

/**
 * Mirror酱错误码对应表
 */
export interface MirrorcErrorInfo {
  code: number;
  message: string;
  showSourceDialog?: boolean;
}

export const MIRRORC_ERROR_CODES: Record<number, MirrorcErrorInfo> = {
  1001: {
    code: 1001,
    message: 'Mirror酱参数错误，请检查打包配置',
  },
  7001: {
    code: 7001,
    message: 'Mirror酱 CDK 已过期',
    showSourceDialog: true,
  },
  7002: {
    code: 7002,
    message: 'Mirror酱 CDK 错误，请检查设置的 CDK 是否正确',
    showSourceDialog: true,
  },
  7003: {
    code: 7003,
    message: 'Mirror酱 CDK 今日下载次数已达上限，请更换 CDK 或明天再试',
  },
  7004: {
    code: 7004,
    message: 'Mirror酱 CDK 类型和待下载的资源不匹配，请检查设置的 CDK 是否正确',
    showSourceDialog: true,
  },
  7005: {
    code: 7005,
    message: 'Mirror酱 CDK 已被封禁，请更换 CDK',
    showSourceDialog: true,
  },
  8001: {
    code: 8001,
    message: '从Mirror酱获取更新失败，请检查打包配置',
  },
  8002: {
    code: 8002,
    message: 'Mirror酱参数错误，请检查打包配置',
  },
  8003: {
    code: 8003,
    message: 'Mirror酱参数错误，请检查打包配置',
  },
  8004: {
    code: 8004,
    message: 'Mirror酱参数错误，请检查打包配置',
  },
};

/**
 * 获取Mirror酱错误信息
 * @param code 错误码
 * @returns 错误信息，如果不是已知错误码则返回null
 */
export function getMirrorcErrorInfo(code: number): MirrorcErrorInfo | null {
  return MIRRORC_ERROR_CODES[code] || null;
}

/**
 * 处理Mirror酱错误并记录日志
 * @param mirrorcStatus Mirror酱状态响应
 * @param contextType 错误上下文类型（用于日志区分）
 * @returns 处理后的错误信息
 */
export function processMirrorcError(
  mirrorcStatus: { code: number; msg?: string },
  contextType: 'install' | 'cdk-validation' = 'install'
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
    // 记录已知错误码
    error(`Mirror酱${contextType === 'cdk-validation' ? 'CDK验证' : ''}错误 [${mirrorcStatus.code}]: ${errorInfo.message}`);
    
    return {
      isError: true,
      errorInfo,
      message: errorInfo.message,
      showSourceDialog: errorInfo.showSourceDialog || false
    };
  } else {
    // 处理未知错误码
    const unknownMessage = contextType === 'cdk-validation' 
      ? `从Mirror酱获取CDK状态失败: ${mirrorcStatus.msg || '未知错误'}，请联系Mirror酱客服`
      : `从Mirror酱获取更新失败: ${mirrorcStatus.msg || '未知错误'}，请联系Mirror酱客服`;
    
    // 记录未知错误码
    error(`Mirror酱${contextType === 'cdk-validation' ? 'CDK验证' : ''}未知错误 [${mirrorcStatus.code}]: ${mirrorcStatus.msg || '无详细信息'}`);
    
    return {
      isError: true,
      errorInfo: {
        code: mirrorcStatus.code,
        message: unknownMessage
      },
      message: unknownMessage,
      showSourceDialog: false
    };
  }
}
