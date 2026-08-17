import { pluginManager } from './index';
import { GitHubPlugin } from './github';

let registered = false;

export function registerAllPlugins() {
  if (registered) {
    return;
  }
  registered = true;
  pluginManager.register(new GitHubPlugin());
}