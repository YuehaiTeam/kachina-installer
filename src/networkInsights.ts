import { log } from './api/ipc';
import { InsightItem } from './types';

// 全局统计数据数组
export const networkInsights: InsightItem[] = [];

// 添加统计数据
export function addNetworkInsight(insight: InsightItem) {
  networkInsights.push(insight);
  // 不再单独记录日志，将由任务完成时统一记录
}

// 清空统计数据
export function clearNetworkInsights() {
  networkInsights.length = 0;
}
