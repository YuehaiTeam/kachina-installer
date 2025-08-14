import type { KachinaInstallSource } from '../types';
import type { HttpGetResponse } from '../../types';
import { invoke } from '../../tauri';

export class GitHubPlugin implements KachinaInstallSource {
  name = 'github';
  private versionCache = new Map<
    string,
    {
      version: string;
      resolvedUrl: string;
    }
  >();

  matchUrl(url: string): boolean {
    return url.includes('https://github.com') && url.includes('${version}');
  }

  private parseUrl(url: string) {
    const [baseUrl, params] = url.split('#');

    let versionRegex: string | undefined = undefined;

    // 如果有参数，尝试获取自定义正则
    if (params) {
      const searchParams = new URLSearchParams(params);
      const customRegex = searchParams.get('versionRegex');
      if (customRegex) {
        versionRegex = customRegex;
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

    return {
      baseUrl,
      versionRegex,
      owner,
      repo,
      releasesLatestUrl,
      cacheKey: `${owner}/${repo}`,
    };
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
    const { baseUrl, versionRegex, releasesLatestUrl, cacheKey } = this.parseUrl(url);

    let cached = this.versionCache.get(cacheKey);

    if (!cached) {
      const version = await this.resolveVersion(releasesLatestUrl, versionRegex);
      const resolvedUrl = baseUrl.replace(/\$\{version\}/g, version);
      cached = { version, resolvedUrl };
      this.versionCache.set(cacheKey, cached);
    }

    return {
      url: cached.resolvedUrl,
      range: range,
    };
  }
}
