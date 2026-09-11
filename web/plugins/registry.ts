import { pluginManager } from './index';
import { GitHubPlugin } from './github';
import { StubPlugin } from './stub';

let registered = false;

export function registerAllPlugins() {
  if (registered) {
    return;
  }
  registered = true;
  pluginManager.register(new GitHubPlugin());
  pluginManager.register(new StubPlugin());
}
