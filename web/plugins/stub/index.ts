import type { KachinaInstallSource } from '../types';

export class StubPlugin implements KachinaInstallSource {
  name = 'stub';

  matchUrl(_url: string): boolean {
    return false;
  }

  async getChunkUrl(
    url: string,
    range: string,
  ): Promise<{ url: string; range: string }> {
    const parsed = new URL(url);
    parsed.searchParams.set('from', 'stub');
    return {
      url: parsed.toString(),
      range,
    };
  }
}
