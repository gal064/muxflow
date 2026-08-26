import { requireNativeModule } from "expo-modules-core";

import type { NativeMuxflowSshModule } from "../../src/ssh/MuxflowSsh";

/**
 * The raw native module. Application code should not use this directly — `src/ssh/MuxflowSsh.ts`
 * wraps it in the typed facade described in design doc §6.1.
 */
export default requireNativeModule<NativeMuxflowSshModule>("MuxflowSsh");
