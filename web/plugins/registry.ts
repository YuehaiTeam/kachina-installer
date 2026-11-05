import { pluginManager } from './index';
import { StubPlugin } from './stub';

let registered = false;

export function registerAllPlugins() {
  if (registered) {
    return;
  }
  registered = true;
  pluginManager.register(new StubPlugin());
}
