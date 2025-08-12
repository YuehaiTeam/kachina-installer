import { DfsUpdateTask, VirtualMergedFile, DfsMetadataHashType, Embedded } from './types';
import { runDfsDownload, runMergedGroupDownload, getFileInstallMode } from './dfs';
import { log, warn, error } from './api/ipc';
import { networkInsights } from './networkInsights';

// 格式化文件大小
const formatFileSize = (size: number): string => {
  if (size >= 1024 * 1024) {
    return `${(size / 1024 / 1024).toFixed(1)}MB`;
  }
  return `${(size / 1024).toFixed(0)}KB`;
};

// 输出统一格式的任务日志
const logTaskResult = (
  file: DfsUpdateTask, 
  mode: string, 
  isSuccess: boolean, 
  errorMsg?: string
) => {
  const size = formatFileSize(file.size);
  const filename = file.file_name;
  
  // 获取最新的网络洞察数据（过滤出与当前文件相关的）
  const recentInsights = networkInsights.slice(-1); // 获取最近的一条
  const insightsJson = recentInsights.length > 0 ? JSON.stringify(recentInsights[0]) : '{}';
  
  if (isSuccess) {
    log(`[${mode}] ${size} ${filename} ${insightsJson}`);
  } else {
    error(`[${mode}] ${size} ${filename} ${errorMsg} ${insightsJson}`);
  }
};

// 输出合并文件的单个文件日志（不含insights）
const logMergedFileResult = (
  file: DfsUpdateTask,
  mode: string,
  isSuccess: boolean,
  errorMsg?: string
) => {
  const size = formatFileSize(file.size);
  const filename = file.file_name;
  
  if (isSuccess) {
    log(`[${mode}-MERGED] ${filename} ${size}`);
  } else {
    error(`[${mode}-MERGED] ${filename} ${size} ${errorMsg}`);
  }
};

// 输出合并组的汇总日志
const logMergedGroupResult = (
  files: DfsUpdateTask[],
  isSuccess: boolean,
  errorMsg?: string
) => {
  const fileNames = files.map(f => f.file_name).join(',');
  
  // 获取最新的网络洞察数据
  const recentInsights = networkInsights.slice(-1);
  const insightsJson = recentInsights.length > 0 ? JSON.stringify(recentInsights[0]) : '{}';
  
  if (isSuccess) {
    log(`[MERGED] ${fileNames} ${insightsJson}`);
  } else {
    error(`[MERGED] ${fileNames} ${errorMsg} ${insightsJson}`);
  }
};

// 释放上下文信息
export interface DownloadContext {
  dfsSource: string;
  extras: string | undefined;
  local: Embedded[];
  source: string;
  hashKey: DfsMetadataHashType;
  elevate: boolean;
}

// 释放任务接口
export interface DownloadTask {
  getSize(): number;
  getDisplayName(): string;
  execute(): Promise<void>;
}

// 单文件释放任务
export class SingleFileTask implements DownloadTask {
  constructor(
    private file: DfsUpdateTask,
    private context: DownloadContext,
    private taskManager?: DownloadTaskManager
  ) {}

  getSize(): number {
    return this.file.size;
  }

  getDisplayName(): string {
    return this.file.file_name;
  }

  async execute(): Promise<void> {
    try {
      await runDfsDownload(
        this.context.dfsSource,
        this.context.extras,
        this.context.local,
        this.context.source,
        this.context.hashKey,
        this.file,
        this.file.failed,
        this.file.failed || false,
        this.context.elevate,
      );
      
      // 成功：确定文件模式并记录
      const mode = getFileInstallMode(this.file, this.context.local, this.context.hashKey);
      logTaskResult(this.file, mode.toUpperCase(), true);
    } catch (error) {
      // 失败：确定文件模式并记录错误
      const mode = getFileInstallMode(this.file, this.context.local, this.context.hashKey);
      logTaskResult(this.file, mode.toUpperCase(), false, String(error));
      throw error;
    }
  }
}

// Local文件释放任务（从内嵌数据释放）
export class LocalFileTask implements DownloadTask {
  constructor(
    private file: DfsUpdateTask,
    private context: DownloadContext
  ) {}

  getSize(): number {
    return this.file.size;
  }

  getDisplayName(): string {
    return this.file.file_name;
  }

  // 标识这是local任务
  isLocalTask(): boolean {
    return true;
  }

  async execute(): Promise<void> {
    try {
      await runDfsDownload(
        this.context.dfsSource,
        this.context.extras,
        this.context.local,
        this.context.source,
        this.context.hashKey,
        this.file,
        this.file.failed,
        this.file.failed || false,
        this.context.elevate,
      );
      
      // 成功：Local文件总是LOCAL模式
      logTaskResult(this.file, 'LOCAL', true);
    } catch (error) {
      // 失败：Local文件记录错误
      logTaskResult(this.file, 'LOCAL', false, String(error));
      throw error;
    }
  }
}

// 合并组释放任务
export class MergedGroupTask implements DownloadTask {
  constructor(
    private virtualFile: VirtualMergedFile,
    private context: DownloadContext,
    private taskManager?: DownloadTaskManager
  ) {}

  getSize(): number {
    return this.virtualFile._mergedInfo.totalEffectiveSize;
  }

  getDisplayName(): string {
    return `合并组(${this.virtualFile._mergedInfo.files.length}个文件)`;
  }

  async execute(): Promise<void> {
    try {
      await runMergedGroupDownload(
        this.virtualFile._mergedInfo,
        this.context.dfsSource,
        this.context.extras,
        this.context.local,
        this.context.source,
        this.context.hashKey,
        this.context.elevate,
      );
      
      // 成功：为每个文件输出日志，并输出汇总日志
      this.logMergedResults(true);
      
    } catch (error) {
      // 整个合并失败：输出汇总错误日志
      this.logMergedResults(false, String(error));
      
      // Fallback: 将内部文件动态添加到队列
      if (this.taskManager) {
        // 重置fallback文件状态
        this.virtualFile._fallbackFiles.forEach(f => {
          f.running = false;
          f.downloaded = 0;
          f.failed = undefined;
        });
        
        const fallbackTasks = this.virtualFile._fallbackFiles.map(
          file => new SingleFileTask(file, this.context, this.taskManager)
        );
        
        fallbackTasks.forEach(task => this.taskManager!.addTask(task));
        
        // 不抛出错误，让 TaskManager 处理 fallback 任务
        // 如果 fallback 任务失败，会在它们的 execute 中抛出错误
      } else {
        // 如果没有taskManager，抛出错误让外层处理
        throw error;
      }
    }
  }
  
  // 处理合并下载的日志输出
  private logMergedResults(isSuccess: boolean, errorMsg?: string) {
    if (isSuccess) {
      // 检查每个文件的状态
      let hasAnyFailure = false;
      
      this.virtualFile._mergedInfo.files.forEach(file => {
        const mode = getFileInstallMode(file, this.context.local, this.context.hashKey);
        
        if (file.failed && (file as any).errorMessage) {
          // 单个文件失败
          logMergedFileResult(file, mode.toUpperCase(), false, (file as any).errorMessage);
          hasAnyFailure = true;
        } else {
          // 单个文件成功
          logMergedFileResult(file, mode.toUpperCase(), true);
        }
      });
      
      // 输出汇总日志
      logMergedGroupResult(this.virtualFile._mergedInfo.files, true);
    } else {
      // 整个合并失败
      logMergedGroupResult(this.virtualFile._mergedInfo.files, false, errorMsg);
    }
  }
}

// 释放任务管理器
export class DownloadTaskManager {
  private largeTaskQueue: Array<DownloadTask> = [];
  private smallTaskQueue: Array<DownloadTask> = [];
  private localTaskQueue: Array<DownloadTask> = [];  // 新增：local文件独立队列
  private largeTaskRunning = 0;
  private smallTaskRunning = 0;
  private localTaskRunning = 0;  // 新增：local任务运行计数
  private allTasks = new Set<DownloadTask>();
  private completedTasks = new Set<DownloadTask>();
  private failedTasks = new Set<DownloadTask>();

  private readonly LARGE_CONCURRENT = 4;
  private readonly SMALL_CONCURRENT = 6;
  private readonly LOCAL_CONCURRENT = 16;  // 新增：local文件并发数
  private sizeThreshold: number;

  // 解析任务完成的Promise
  private resolveCompletion?: () => void;
  private completionPromise?: Promise<void>;

  constructor(files: (DfsUpdateTask | VirtualMergedFile)[] = []) {
    this.sizeThreshold = this.calculateOptimalThreshold(files);
    log('TaskManager initialized:', {
      threshold: (this.sizeThreshold / 1024 / 1024).toFixed(1) + 'MB',
      largeSlots: this.LARGE_CONCURRENT,
      smallSlots: this.SMALL_CONCURRENT,
      localSlots: this.LOCAL_CONCURRENT,
      totalFiles: files.length
    });
  }

  // 计算最优阈值
  private calculateOptimalThreshold(files: (DfsUpdateTask | VirtualMergedFile)[]): number {
    if (files.length === 0) return 1024 * 1024; // 默认1MB

    const sizes = files.map(f => {
      if ((f as VirtualMergedFile)._isMergedGroup) {
        return (f as VirtualMergedFile)._mergedInfo.totalEffectiveSize;
      }
      return f.size;
    }).sort((a, b) => b - a);

    if (sizes.length <= 3) return 0;

    // 目标：让大文件数量在2-4个之间
    const targetLargeFiles = Math.min(4, Math.max(2, Math.floor(sizes.length * 0.3)));
    const thresholdIndex = Math.min(targetLargeFiles, sizes.length - 1);
    
    return sizes[thresholdIndex] * 0.8; // 稍微降低阈值确保分类合理
  }

  // 添加任务到相应队列
  addTask(task: DownloadTask): void {
    this.allTasks.add(task);
    
    // 检查是否为local任务
    if ((task as any).isLocalTask && (task as any).isLocalTask()) {
      this.localTaskQueue.push(task);
    } else if (task.getSize() >= this.sizeThreshold) {
      this.largeTaskQueue.push(task);
    } else {
      this.smallTaskQueue.push(task);
    }
    
    this.tryStartTasks();
  }

  // 尝试启动待处理任务
  private tryStartTasks(): void {
    // 启动local文件任务（最高并发度）
    while (this.localTaskRunning < this.LOCAL_CONCURRENT && this.localTaskQueue.length > 0) {
      const task = this.localTaskQueue.shift()!;
      this.localTaskRunning++;
      this.executeTask(task, 'local');
    }

    // 启动大文件任务
    while (this.largeTaskRunning < this.LARGE_CONCURRENT && this.largeTaskQueue.length > 0) {
      const task = this.largeTaskQueue.shift()!;
      this.largeTaskRunning++;
      this.executeTask(task, 'large');
    }

    // 启动小文件任务
    while (this.smallTaskRunning < this.SMALL_CONCURRENT && this.smallTaskQueue.length > 0) {
      const task = this.smallTaskQueue.shift()!;
      this.smallTaskRunning++;
      this.executeTask(task, 'small');
    }
  }

  // 执行任务（带重试机制）
  private async executeTask(task: DownloadTask, type: 'large' | 'small' | 'local'): Promise<void> {
    const maxRetries = 3;
    let lastError: any = null;

    try {
      for (let attempt = 1; attempt <= maxRetries; attempt++) {
        try {
          await task.execute();
          this.completedTasks.add(task);
          // 成功：统一日志格式将在task.execute()内部处理
          return; // 成功，退出重试循环
        } catch (error) {
          lastError = error;
          
          if (attempt === maxRetries) {
            // 所有重试都失败了
            this.failedTasks.add(task);
            // 失败：统一日志格式将在task.execute()内部处理
            // 停止安装流程，使用用户友好的错误格式
            throw new Error(`释放文件 ${task.getDisplayName()} 失败：\n${lastError}`);
          }
        }
      }
    } finally {
      // 减少对应类型的运行计数
      if (type === 'large') {
        this.largeTaskRunning--;
      } else if (type === 'small') {
        this.smallTaskRunning--;
      } else if (type === 'local') {
        this.localTaskRunning--;
      }
      
      // 检查是否所有任务完成
      this.checkCompletion();
      
      // 继续处理队列
      this.tryStartTasks();
    }
  }

  // 检查是否所有任务完成
  private checkCompletion(): void {
    const totalProcessed = this.completedTasks.size + this.failedTasks.size;
    const allQueuesEmpty = this.largeTaskQueue.length === 0 && 
                           this.smallTaskQueue.length === 0 && 
                           this.localTaskQueue.length === 0;
    const noRunningTasks = this.largeTaskRunning === 0 && 
                          this.smallTaskRunning === 0 && 
                          this.localTaskRunning === 0;

    if (totalProcessed === this.allTasks.size && allQueuesEmpty && noRunningTasks) {
      log('All tasks completed');
      if (this.resolveCompletion) {
        this.resolveCompletion();
        this.resolveCompletion = undefined;
      }
    }
  }

  // 等待所有任务完成
  async waitForCompletion(): Promise<void> {
    if (this.allTasks.size === 0) return;

    if (!this.completionPromise) {
      this.completionPromise = new Promise<void>((resolve) => {
        this.resolveCompletion = resolve;
        this.checkCompletion(); // 立即检查一次
      });
    }

    return this.completionPromise;
  }

  // 获取统计信息
  getStats() {
    return {
      total: this.allTasks.size,
      completed: this.completedTasks.size,
      failed: this.failedTasks.size,
      largeRunning: this.largeTaskRunning,
      smallRunning: this.smallTaskRunning,
      localRunning: this.localTaskRunning,
      largeQueued: this.largeTaskQueue.length,
      smallQueued: this.smallTaskQueue.length,
      localQueued: this.localTaskQueue.length,
      threshold: this.sizeThreshold
    };
  }
}