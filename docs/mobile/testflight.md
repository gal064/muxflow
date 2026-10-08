# TestFlight releases

Muxflow uses internal and external TestFlight, with foreground SSH
and local notifications. It needs no APNs credentials or Push Notifications
capability. The App Store Connect record uses `dev.muxflow.mobile`.

## Credentials

The GitHub `ios-testflight` environment holds:

- `MUXFLOW_IOS_CERTIFICATE_P12_BASE64`: Apple Distribution certificate and key.
- `MUXFLOW_IOS_CERTIFICATE_PASSWORD`: the P12 export password.
- `MUXFLOW_IOS_PROVISIONING_PROFILE_BASE64`: App Store distribution profile.
- `MUXFLOW_APPLE_TEAM_ID`: Apple Developer membership team.
- `MUXFLOW_APP_STORE_CONNECT_KEY_P8_BASE64`: team API key with App Manager access.
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

## Release to testers

Push a reviewed version tag to build all platforms. The release workflow signs
and notarizes the Mac app, uploads the signed iOS build, waits for Apple to
finish processing it, and assigns it to the internal group. The GitHub release
stays a draft until you publish it. RC tags may point to a branch; stable tags
must point to `main`.

The `ios-testflight` environment has `MUXFLOW_TESTFLIGHT_INTERNAL_GROUP=Checksum`
and `MUXFLOW_TESTFLIGHT_EXTERNAL_GROUP=Beta`. Manage testers in these existing
App Store Connect groups; no invitations or public links are created by CI.

For external testing, first complete Muxflow → TestFlight → Test Information in
App Store Connect: beta description, feedback email, review contact, sign-in
choice, and instructions that let Apple test the SSH connection. Provide a
reachable review host and credentials there if needed. The workflow checks
that the required fields are filled; only Apple can judge whether they suffice.

Then open GitHub Actions → **Distribute TestFlight build** → **Run workflow**,
enter the uploaded build number, choose `external`, and enter What to Test.
It uses the existing signed build, assigns it to Beta, submits beta review if
needed, and enables distribution to testers after approval. Pending reviews
are not submitted again. Apple can require review for later builds too.
See [Apple’s external testing instructions](https://developer.apple.com/help/app-store-connect/test-a-beta-version/invite-external-testers/).
The Actions button is available after this workflow reaches the default branch.
For an existing build on a reviewed ref, the equivalent command is:

```bash
gh workflow run testflight-distribute.yml --ref main \
  -f build_number=3 -f audience=external -f notes='Test terminal scrolling and reconnect.'
```

The internal and external jobs refuse unfinished, expired or non-compliant
builds and refuse a group with the wrong audience. A failure after upload does
not require rebuilding: use **Distribute TestFlight build** with the retained
build number after resolving the error.

## Build and upload

1. Review the source and complete simulator QA for app behavior changes.
2. Allocate a new build number using `release/set-version.sh 0.1.13 3` (replace
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
   It waits for Apple’s processing state `VALID`, retains the build record,
   and assigns it to the configured internal group. Device QA remains separate.

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
does not implement proprietary encryption. The account holder completed Apple's
questionnaire for the current encryption and distribution outside France;
Apple recorded `usesNonExemptEncryption: false` on build 2. New builds record
that declaration as `ITSAppUsesNonExemptEncryption: false` so the same questions
do not block every upload. This declares an exemption from documentation,
not an absence of encryption. Reassess before changing encryption or extending
distribution to France. See Apple's [encryption documentation requirements](https://developer.apple.com/help/app-store-connect/reference/app-information/export-compliance-documentation-for-encryption).

After processing and compliance, create an Internal Testing group, add your
App Store Connect user and the build, then accept the invitation in TestFlight
on the iPhone. See [Apple's internal testing instructions](https://developer.apple.com/help/app-store-connect/test-a-beta-version/add-internal-testers/).
Test LAN permission, SSH authentication, terminal interaction, brief app
switches, lock/resume, and microphone/audio on the physical device. Simulator
evidence cannot close those device gates.

## First signed upload evidence

[Run 37434806475](https://github.com/gal064/muxflow/actions/runs/37434806475)
successfully built and uploaded `0.1.9 (2)` from candidate `bebfc43` on
2026-10-06. Apple build/upload ID is
`bf468001-025c-44c6-ae1f-278de5a3b782`, with processing state `VALID`.
Apple validation and upload completed without errors.

Independent QA verified the retained IPA's metadata, App Store profile,
signed binary entitlements and digest, with no push/background modes. The
runner passed strict/deep native signature verification. The IPA SHA-256 is
`3836046445113c1b37295c0f4cf3bc0855c689194b66b57a1954770944351951`.

At initial verification, Apple reported `MISSING_EXPORT_COMPLIANCE` and
`usesNonExemptEncryption: null`. After the account holder completed the
build-level compliance form, live API verification confirms
`usesNonExemptEncryption: false`, processing state `VALID` and internal state
`READY_FOR_BETA_TESTING`. Credential setup, signed build/upload automation,
encryption compliance and Apple upload/processing (steps 1–4) are complete.
Internal tester setup and real-iPhone QA remain separate installation/device
gates.
