import type { KachinaInstallSource } from './types';

export class PluginManager {
  private plugins: KachinaInstallSource[] = [];
  
  register(plugin: KachinaInstallSource): void {
    this.plugins.push(plugin);
  }
  
  private parseUrl(url: string): { cleanUrl: string | null; forcedPlugin: string | null } {
    // 检查是否包含dfs+或dfs2+，如果有则不匹配插件
    if (url.includes('dfs+') || url.includes('dfs2+')) {
      return { cleanUrl: null, forcedPlugin: null };
    }
    
    // 找到://前的内容进行分析
    const protocolIndex = url.indexOf('://');
    if (protocolIndex === -1) return { cleanUrl: url, forcedPlugin: null };
    
    const beforeProtocol = url.substring(0, protocolIndex);
    const afterProtocol = url.substring(protocolIndex);
    
    // 检查是否有plugin-强制指定格式
    const pluginMatch = beforeProtocol.match(/plugin-([^+]+)\+(.*)$/);
    if (pluginMatch) {
      const [, pluginName, remainingPrefix] = pluginMatch;
      // 重新组装URL，移除plugin-xxx+部分
      const cleanUrl = remainingPrefix ? `${remainingPrefix}${afterProtocol}` : `https${afterProtocol}`;
      return { cleanUrl, forcedPlugin: pluginName };
    }
    
    // 普通处理：找到最后一个+，过滤前面的内容
    const lastPlusIndex = beforeProtocol.lastIndexOf('+');
    const cleanUrl = lastPlusIndex === -1 
      ? url 
      : beforeProtocol.substring(lastPlusIndex + 1) + afterProtocol;
    
    return { cleanUrl, forcedPlugin: null };
  }
  
  private extractCleanUrl(url: string): string | null {
    return this.parseUrl(url).cleanUrl;
  }
  
  findPlugin(url: string): KachinaInstallSource | null {
    const { cleanUrl, forcedPlugin } = this.parseUrl(url);
    if (!cleanUrl) return null;
    
    // 如果指定了强制插件，直接按名称查找
    if (forcedPlugin) {
      const plugin = this.plugins.find(p => p.name === forcedPlugin);
      if (!plugin) {
        throw new Error(`Plugin "${forcedPlugin}" not found`);
      }
      return plugin;
    }
    
    // 否则按URL匹配
    return this.plugins.find(plugin => plugin.matchUrl(cleanUrl)) || null;
  }
  
  isPluginSource(url: string): boolean {
    return this.findPlugin(url) !== null;
  }
  
  getCleanUrl(url: string): string | null {
    return this.extractCleanUrl(url);
  }
}

export const pluginManager = new PluginManager();

// 导出类型供其他模块使用
export type { KachinaInstallSource } from './types';