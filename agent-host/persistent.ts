import type { Bootstrap } from './bootstrap.js';
import { PersistentHost as OwnedPersistentHost, type Hooks } from './persistent-core.js';
import { Session } from './session.js';

export class PersistentHost extends OwnedPersistentHost {
  constructor(config: Bootstrap, hooks: Hooks = {}) {
    super(config, (configuration) => new Session(configuration), hooks);
  }
}

export { listed, HOST_CAPABILITIES } from './persistent-core.js';
