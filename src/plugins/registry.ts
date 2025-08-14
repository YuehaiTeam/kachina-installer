import { pluginManager } from './index';
import { GitHubPlugin } from './github';

export function registerAllPlugins() {
  pluginManager.register(new GitHubPlugin());
}