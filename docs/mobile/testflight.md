# Internal TestFlight

Muxflow's first iOS distribution is internal TestFlight, with foreground SSH
and local notifications. It needs no APNs credentials or Push Notifications
capability. The App Store Connect record uses `dev.muxflow.mobile`.

## Credentials

The GitHub `ios-testflight` environment holds:

- `MUXFLOW_IOS_CERTIFICATE_P12_BASE64`: Apple Distribution certificate and key.
- `MUXFLOW_IOS_CERTIFICATE_PASSWORD`: the P12 export password.
- `MUXFLOW_IOS_PROVISIONING_PROFILE_BASE64`: App Store distribution profile.
- `MUXFLOW_APPLE_TEAM_ID`: Apple Developer membership team.
- `MUXFLOW_APP_STORE_CONNECT_KEY_P8_BASE64`: team API key with Developer access.
- `MUXFLOW_APP_STORE_CONNECT_KEY_ID` and `MUXFLOW_APP_STORE_CONNECT_ISSUER_ID`.

Private keys and passwords stay outside the repository. The macOS runner imports
the signing identity into a temporary keychain and removes it and the installed
profile after export. API key material exists only for validation/upload.
P12 packages created with OpenSSL 3 defaults may fail Apple's importer with
a MAC-verification error despite a valid password. Package the existing
identity using macOS-compatible PKCS12 encryption (PBESv1 SHA-1/3DES with a
SHA-1 MAC) and keep it protected by encrypted GitHub Secrets and a strong
export password. Certificate import runs before prebuild/pod installation
so an import failure stops promptly.

## Build and upload

1. Review the source and complete simulator QA for app behavior changes.
2. Allocate a new build number using `release/set-version.sh 0.1.9 2` (replace
   both values with the intended version and a number greater than the current
   number). Commit it with the candidate. The protocol major is independent:
   this distribution change does not bump it.
3. Dispatch the existing `ios` workflow on the reviewed candidate ref, setting
   `testflight=true`. Ordinary PR/push runs still run simulator QA and have no
   access to signing credentials. Example:

   ```bash
   gh workflow run ios.yml --ref feat/ios-mobile -f testflight=true
   ```

4. The runner first checks App Store Connect's processed builds and upload
   history, refusing a build number Apple already has even during processing.
   It then generates the native project, signs only the app
   target, archives and exports an App Store IPA. It verifies signatures,
   recorded versions, team, profile, permission strings and absence of push
   or development entitlements before retaining the IPA and SHA-256 manifest.
5. Apple's `altool` validates that exact IPA, then uploads it using the API key.
   It waits for Apple's build record and retains its identifier/status.
   Upload success does not mean processing, compliance or device QA passed.

Do not rerun a signed build under an uploaded build number. Workflow reruns are
refused. If only upload failed, download the retained IPA and `manifest.json`
artifact into `tmp/release/ios` on a Mac, provide the API credentials and
`RUNNER_TEMP`, and run `uv run --no-project release/ios/testflight.py upload`.
It verifies the artifact digest. A changed binary needs a new number and run.
If an upload was accepted but its Apple record is not yet visible, wait for
processing and inspect App Store Connect before dispatching again.

## Encryption declaration and installation

Muxflow uses standard SSH encryption via bundled libssh2 and OpenSSL, in
addition to Apple's Keychain/CryptoKit. It is not limited to OS encryption and
does not implement proprietary encryption. The build leaves
`ITSAppUsesNonExemptEncryption` unset until the account holder completes Apple's
questionnaire; upload can succeed while the build shows Missing Compliance.
Do not answer that the app uses no encryption or only OS encryption.

In App Store Connect, open Muxflow → TestFlight → the build → Manage / Provide
Export Compliance Information. The current Developer-role API key cannot
complete this account-holder/App Manager step. Apple lists a French declaration
for standard encryption outside the OS when distributing on the App Store in
France; confirm intended distribution and any required documents in the
questionnaire before setting an exemption or compliance code in future builds.
See Apple's [encryption documentation requirements](https://developer.apple.com/help/app-store-connect/reference/app-information/export-compliance-documentation-for-encryption)
and [beta compliance instructions](https://developer.apple.com/help/app-store-connect/test-a-beta-version/provide-export-compliance-information-for-beta-builds/).

After processing and compliance, create an Internal Testing group, add your
App Store Connect user and the build, then accept the invitation in TestFlight
on the iPhone. See [Apple's internal testing instructions](https://developer.apple.com/help/app-store-connect/test-a-beta-version/add-internal-testers/).
Test LAN permission, SSH authentication, terminal interaction, brief app
switches, lock/resume, and microphone/audio on the physical device. Simulator
evidence cannot close those device gates.
