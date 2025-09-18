export const friendlyError = (error: string | { message: string }): string => {
  const errStr = typeof error === 'string' ? error : error.message;
  // 空格，换行符，制表符，右括号，逗号都是url结束
  const firstUrlInstr = errStr.match(/https?:\/\/[^\s),]+/);
  // 替换url时保留url结束标志字符，避免把右括号等也替换掉
  const errStrWithoutUrl = errStr.replace(/https?:\/\/[^\s),]+/g, '[url]');
  let friendlyStr = '';
  const checkStr = errStrWithoutUrl.toLowerCase();
  if (errStr.includes('operation timed out')) {
    friendlyStr = '连接下载服务器超时，请检查你的网络连接或更换下载源';
  } else if (checkStr.includes('connection refused')) {
    friendlyStr = '下载服务器出现问题，请重试或更换下载源';
  } else if (checkStr.includes('connection reset')) {
    friendlyStr = '连接下载服务器失败，请重试或更换下载源';
  } else if (checkStr.includes('too_slow') || checkStr.includes('stalled')) {
    friendlyStr = '检测到下载速度异常，请检查你的网络连接或更换下载源';
  }

  if (friendlyStr) {
    return `${friendlyStr}\n\n原始错误：${errStrWithoutUrl}${firstUrlInstr ? `\n\n下载服务器：${firstUrlInstr[0]}` : ''}`;
  } else {
    return `${errStrWithoutUrl}${firstUrlInstr ? `\n\n下载服务器：${firstUrlInstr[0]}` : ''}`;
  }
};
