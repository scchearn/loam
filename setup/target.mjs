export {
  SUPPORTED_TARGETS,
  assertInside,
  detectTarget,
  resolveGlobalRoot,
  resolveSkillsRoot,
  runtimePath,
} from '../integration/paths.mjs';

import { SUPPORTED_TARGETS } from '../integration/paths.mjs';

export function assertSupportedTarget(target) {
  if (!SUPPORTED_TARGETS.includes(target)) throw new Error(`unsupported runtime target: ${target}`);
  return target;
}
