// Edit only the generated application target, never the CocoaPods targets.
const fs = require("node:fs");
const path = require("node:path");
const mobile = path.resolve(__dirname, "../../apps/mobile");
const plugins = require.resolve("expo/config-plugins", { paths: [mobile] });
const xcode = require(require.resolve("xcode", { paths: [plugins] }));
const filename = path.join(mobile, "ios/Muxflow.xcodeproj/project.pbxproj");
const project = xcode.project(filename);
project.parseSync();
let changed = 0;
for (const config of Object.values(project.pbxXCBuildConfigurationSection())) {
  const settings = config.buildSettings;
  if (!settings || settings.PRODUCT_BUNDLE_IDENTIFIER?.replaceAll('"', '') !== "dev.muxflow.mobile") continue;
  settings.CODE_SIGN_STYLE = "Manual";
  settings.DEVELOPMENT_TEAM = process.env.MUXFLOW_APPLE_TEAM_ID;
  settings.CODE_SIGN_IDENTITY = '"Apple Distribution"';
  settings.PROVISIONING_PROFILE_SPECIFIER = process.env.MUXFLOW_IOS_PROFILE_UUID;
  changed++;
}
if (changed !== 2) throw new Error(`Expected two app signing configurations, found ${changed}`);
fs.writeFileSync(filename, project.writeSync());
