const { withEntitlementsPlist } = require("expo/config-plugins");

// expo-notifications adds APNs even when only local notifications are used.
// Foreground SSH notifications do not need Push Notifications provisioning.
// Register before expo-notifications: its entitlements mod runs first, then
// this mod removes the default (the wrappers compose in reverse order).
module.exports = function withLocalNotifications(config) {
  return withEntitlementsPlist(config, (mod) => {
    delete mod.modResults["aps-environment"];
    return mod;
  });
};
