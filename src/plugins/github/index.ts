import type { KachinaInstallSource } from '../types';
import type { HttpGetResponse } from '../../types';
import { invoke } from '../../tauri';

interface UrlCache {
  resolvedUrl: string;
  expiryTime: number; // Unix timestamp
  originalUrl: string;
}

export class GitHubPlugin implements KachinaInstallSource {
  name = 'github';
  private versionCache = new Map<
    string,
    {
      version: string;
      resolvedUrl: string;
    }
  >();
  private urlCache = new Map<string, UrlCache>();

  matchUrl(url: string): boolean {
    return url.includes('https://github.com') && url.includes('${version}');
  }

  private parseUrl(url: string) {
    const [baseUrl, params] = url.split('#');

    let versionRegex: string | undefined = undefined;
    let cacheTime: number | undefined = undefined;

    // 如果有参数，尝试获取自定义正则和缓存时间
    if (params) {
      const searchParams = new URLSearchParams(params);
      const customRegex = searchParams.get('versionRegex');
      if (customRegex) {
        versionRegex = customRegex;
      }
      
      const cacheTimeParam = searchParams.get('cacheTime');
      if (cacheTimeParam) {
        const parsedTime = parseInt(cacheTimeParam, 10);
        if (!isNaN(parsedTime) && parsedTime > 0) {
          cacheTime = parsedTime;
        }
      }
    }

    // 提取到 /releases/ 的前缀
    const releasesIndex = baseUrl.indexOf('/releases/');
    if (releasesIndex === -1) throw new Error('URL must contain /releases/');
    
    const releasesPrefix = baseUrl.substring(0, releasesIndex + '/releases'.length);
    const releasesLatestUrl = `${releasesPrefix}/latest`;

    // 仍然需要提取owner和repo用于缓存键
    const match = baseUrl.match(/\/([^/]+)\/([^/]+)\/releases/);
    if (!match) throw new Error('Invalid releases URL format');
    
    const [, owner, repo] = match;
    
    // 判断是否应该启用缓存
    const urlObj = new URL(baseUrl);
    const shouldCache = urlObj.hostname === 'github.com' || cacheTime !== undefined;

    return {
      baseUrl,
      versionRegex,
      owner,
      repo,
      releasesLatestUrl,
      cacheKey: `${owner}/${repo}`,
      shouldCache,
      cacheTime,
    };
  }

  private extractExpiryFromUrl(url: string): number {
    try {
      const urlObj = new URL(url);
      const searchParams = urlObj.searchParams;
      
      // 尝试从ske参数提取过期时间（GitHub的签名过期时间）
      const ske = searchParams.get('ske');
      if (ske) {
        const expiryTime = new Date(ske).getTime();
        if (!isNaN(expiryTime)) {
          return expiryTime;
        }
      }
      
      // 尝试从se参数提取过期时间（备用）
      const se = searchParams.get('se');
      if (se) {
        const expiryTime = new Date(se).getTime();
        if (!isNaN(expiryTime)) {
          return expiryTime;
        }
      }
    } catch (e) {
      // URL解析失败，返回默认过期时间
    }
    
    // 默认1小时过期
    return Date.now() + 300 * 1000;
  }

  private cleanExpiredCache(): void {
    const now = Date.now();
    for (const [key, cache] of this.urlCache.entries()) {
      if (cache.expiryTime <= now) {
        this.urlCache.delete(key);
      }
    }
  }

  private async resolveDirectUrl(originalUrl: string, cacheTime?: number): Promise<string> {
    // 先清理过期缓存
    this.cleanExpiredCache();
    
    // 检查缓存
    const cached = this.urlCache.get(originalUrl);
    if (cached && cached.expiryTime > Date.now()) {
      return cached.resolvedUrl;
    }
    
    // 发起HTTP请求获取重定向URL
    const response = await invoke<HttpGetResponse>('http_get_request', {
      url: originalUrl,
      ignoreRedirects: true,
    });
    
    // 从Location header或final_url获取重定向地址
    let redirectUrl = response.headers['location'] || response.final_url;
    if (!redirectUrl || redirectUrl === originalUrl) {
      // 没有重定向，直接返回原URL
      return originalUrl;
    }
    
    // 如果重定向URL是相对路径，转换为绝对路径
    if (redirectUrl.startsWith('/')) {
      const baseUrl = new URL(originalUrl);
      redirectUrl = `${baseUrl.protocol}//${baseUrl.host}${redirectUrl}`;
    }
    
    // 计算过期时间
    let expiryTime: number;
    if (cacheTime) {
      expiryTime = Date.now() + cacheTime * 1000;
    } else {
      expiryTime = this.extractExpiryFromUrl(redirectUrl);
    }
    
    // 缓存结果
    this.urlCache.set(originalUrl, {
      resolvedUrl: redirectUrl,
      expiryTime,
      originalUrl,
    });
    
    return redirectUrl;
  }

  private async resolveVersion(
    releasesLatestUrl: string,
    versionRegex?: string,
  ): Promise<string> {
    const response = await invoke<HttpGetResponse>('http_get_request', {
      url: releasesLatestUrl,
      ignoreRedirects: true,
    });

    const redirectUrl = response.final_url || response.headers['location'];
    if (!redirectUrl) {
      throw new Error('No redirect found for GitHub latest release');
    }

    if (!versionRegex) {
      // 默认行为：从 /releases/tag/ 后提取完整tag
      const tagMatch = redirectUrl.match(/\/releases\/tag\/([^/?#]+)/);
      if (!tagMatch || !tagMatch[1]) {
        throw new Error(`Failed to extract tag from ${redirectUrl}`);
      }
      return tagMatch[1];
    } else {
      // 自定义正则：直接对重定向URL应用正则
      const regex = new RegExp(versionRegex);
      const match = redirectUrl.match(regex);

      if (!match || !match[1]) {
        throw new Error(
          `Failed to extract version from ${redirectUrl} using regex ${versionRegex}`,
        );
      }
      return match[1];
    }
  }

  async getChunkUrl(
    url: string,
    range: string,
  ): Promise<{ url: string; range: string }> {
    const { 
      baseUrl, 
      versionRegex, 
      releasesLatestUrl, 
      cacheKey, 
      shouldCache, 
      cacheTime 
    } = this.parseUrl(url);

    let cached = this.versionCache.get(cacheKey);

    if (!cached) {
      const version = await this.resolveVersion(releasesLatestUrl, versionRegex);
      const resolvedUrl = baseUrl.replace(/\$\{version\}/g, version);
      cached = { version, resolvedUrl };
      this.versionCache.set(cacheKey, cached);
    }

    // 如果需要缓存URL解析，进行重定向解析
    if (shouldCache) {
      const finalUrl = await this.resolveDirectUrl(cached.resolvedUrl, cacheTime);
      return {
        url: finalUrl,
        range: range,
      };
    } else {
      // 不缓存，直接返回版本替换后的URL
      return {
        url: cached.resolvedUrl,
        range: range,
      };
    }
  }
}
