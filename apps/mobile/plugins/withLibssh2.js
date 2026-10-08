const { withPodfile } = require("expo/config-plugins");

// A local specification pins the remote library and its small native API patch.
module.exports = function withLibssh2(config) {
  return withPodfile(config, (mod) => {
    const declaration = "pod 'MuxflowLibssh2', :podspec => '../modules/muxflow-ssh/vendor/MuxflowLibssh2.podspec'";
    if (!mod.modResults.contents.includes(declaration)) {
      const target = /target '[^']+' do/;
      if (!target.test(mod.modResults.contents)) throw new Error("iOS Podfile target changed");
      mod.modResults.contents = mod.modResults.contents.replace(target, (match) => `${match}\n  ${declaration}`);
    }
    return mod;
  });
};
