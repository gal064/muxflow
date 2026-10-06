# /// script
# dependencies = ["pyjwt[crypto]==2.10.1"]
# ///
"""Run with uv on a disposable macOS runner. Never print credential values."""

import base64
import datetime
import hashlib
import json
import os
from pathlib import Path
import plistlib
import secrets
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request

REPO = Path(__file__).resolve().parents[2]
MOBILE = REPO / "apps/mobile"
OUTPUT = REPO / "tmp/release/ios"
BUNDLE = "dev.muxflow.mobile"


def require(name):
    value = os.environ.get(name, "")
    if not value:
        raise ValueError(f"Missing {name}")
    return value


def run(args, *, cwd=REPO, capture=False, log=None):
    # CalledProcessError would include password arguments, so check manually.
    if log:
        with (OUTPUT / log).open("wb") as stream:
            result = subprocess.run(args, cwd=cwd, stdout=stream, stderr=subprocess.STDOUT)
    else:
        result = subprocess.run(args, cwd=cwd, stdout=subprocess.PIPE if capture else None)
    if result.returncode:
        raise RuntimeError(f"{Path(args[0]).name} failed ({result.returncode})" + (f"; see {log}" if log else ""))
    return result.stdout if capture else None


def decode_secret(name, destination):
    destination.write_bytes(base64.b64decode(require(name), validate=True))
    destination.chmod(0o600)


def apple_get(path, params):
    import jwt

    now = int(time.time())
    key = base64.b64decode(require("MUXFLOW_APP_STORE_CONNECT_KEY_P8_BASE64"), validate=True)
    token = jwt.encode({"iss": require("MUXFLOW_APP_STORE_CONNECT_ISSUER_ID"), "iat": now, "exp": now + 300, "aud": "appstoreconnect-v1"}, key, algorithm="ES256", headers={"kid": require("MUXFLOW_APP_STORE_CONNECT_KEY_ID"), "typ": "JWT"})
    url = "https://api.appstoreconnect.apple.com/v1/" + path + "?" + urllib.parse.urlencode(params)
    request = urllib.request.Request(url, headers={"Authorization": "Bearer " + token})
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def app_id():
    apps = apple_get("apps", {"filter[bundleId]": BUNDLE, "limit": 2})["data"]
    if len(apps) != 1:
        raise ValueError("Expected one App Store Connect app for the bundle ID")
    return apps[0]["id"]


def find_build(number):
    return apple_get("builds", {"filter[app]": app_id(), "filter[version]": number, "limit": 1})["data"]


def find_upload(number):
    return apple_get(f"apps/{app_id()}/buildUploads", {"filter[cfBundleVersion]": number, "limit": 1})["data"]


def check_number():
    config = json.loads((MOBILE / "app.json").read_text())["expo"]
    number = config["ios"]["buildNumber"]
    if find_build(number) or find_upload(number):
        raise ValueError("Apple already has this build number; reuse the retained IPA or allocate a new number")
    print(f"Apple has no existing build {number}; proceeding with the recorded candidate.", flush=True)


def validate_profile(profile, team):
    entitlement = profile["Entitlements"]
    if profile["TeamIdentifier"] != [team] or entitlement.get("application-identifier") != f"{team}.{BUNDLE}":
        raise ValueError("Provisioning profile belongs to a different team/app")
    if profile["ExpirationDate"] <= datetime.datetime.now(datetime.UTC).replace(tzinfo=None):
        raise ValueError("Provisioning profile expired")
    if entitlement.get("get-task-allow") or profile.get("ProvisionedDevices") or profile.get("ProvisionsAllDevices"):
        raise ValueError("An App Store distribution profile is required")
    if "aps-environment" in entitlement:
        raise ValueError("This milestone uses local notifications; remove Push Notifications from the profile")
    if not profile.get("DeveloperCertificates"):
        raise ValueError("Provisioning profile contains no distribution certificate")


def validate_app(info, entitlement, config, team):
    expected = {"CFBundleIdentifier": BUNDLE, "CFBundleShortVersionString": config["version"], "CFBundleVersion": config["ios"]["buildNumber"]}
    for name, value in expected.items():
        if info.get(name) != value:
            raise ValueError(f"Exported IPA has an unexpected {name}")
    if entitlement.get("application-identifier") != f"{team}.{BUNDLE}" or entitlement.get("com.apple.developer.team-identifier") != team:
        raise ValueError("Exported IPA has an unexpected signing team/app")
    if entitlement.get("get-task-allow") or "aps-environment" in entitlement:
        raise ValueError("Exported IPA has development or push entitlements")
    if info.get("UIBackgroundModes"):
        raise ValueError("This milestone has no background modes")
    if not info.get("NSLocalNetworkUsageDescription") or not info.get("NSMicrophoneUsageDescription"):
        raise ValueError("Exported IPA lacks required permission text")


def build():
    team = require("MUXFLOW_APPLE_TEAM_ID")
    password = require("MUXFLOW_IOS_CERTIFICATE_PASSWORD")
    config = json.loads((MOBILE / "app.json").read_text())["expo"]
    if list(OUTPUT.glob("*.ipa")):
        raise ValueError("An IPA already exists; upload those exact bytes or allocate a new build number")
    run(["bash", "release/check-version.sh"])
    run(["pnpm", "exec", "expo", "prebuild", "--platform", "ios", "--no-install"], cwd=MOBILE, log="prebuild.log")
    run(["pod", "install"], cwd=MOBILE / "ios", log="pods.log")
    original_keychains = run(["security", "list-keychains", "-d", "user"], capture=True).decode().splitlines()
    original_keychains = [line.strip().strip('"') for line in original_keychains]
    installed_profile = None
    with tempfile.TemporaryDirectory(prefix="muxflow-signing-", dir=require("RUNNER_TEMP")) as temp:
        signing = Path(temp)
        keychain = signing / "build.keychain-db"
        keychain_password = secrets.token_hex(24)
        certificate = signing / "distribution.p12"
        profile_file = signing / "profile.mobileprovision"
        try:
            decode_secret("MUXFLOW_IOS_CERTIFICATE_P12_BASE64", certificate)
            decode_secret("MUXFLOW_IOS_PROVISIONING_PROFILE_BASE64", profile_file)
            profile = plistlib.loads(run(["security", "cms", "-D", "-i", str(profile_file)], capture=True))
            validate_profile(profile, team)
            profile_dir = Path.home() / "Library/MobileDevice/Provisioning Profiles"
            profile_dir.mkdir(parents=True, exist_ok=True)
            candidate = profile_dir / f'{profile["UUID"]}.mobileprovision'
            if candidate.exists():
                raise ValueError("Refusing to replace an existing provisioning profile")
            installed_profile = candidate
            shutil.copyfile(profile_file, installed_profile)
            installed_profile.chmod(0o600)
            run(["security", "create-keychain", "-p", keychain_password, str(keychain)])
            run(["security", "set-keychain-settings", "-lut", "21600", str(keychain)])
            run(["security", "unlock-keychain", "-p", keychain_password, str(keychain)])
            run(["security", "import", str(certificate), "-k", str(keychain), "-P", password, "-T", "/usr/bin/codesign", "-T", "/usr/bin/security"])
            run(["security", "set-key-partition-list", "-S", "apple-tool:,apple:", "-s", "-k", keychain_password, str(keychain)], capture=True)
            run(["security", "list-keychains", "-d", "user", "-s", str(keychain), *original_keychains])
            os.environ["MUXFLOW_IOS_PROFILE_UUID"] = profile["UUID"]
            run(["node", "release/ios/configure-signing.cjs"])
            os.environ["NODE_BINARY"] = shutil.which("node")
            archive = OUTPUT / "Muxflow.xcarchive"
            run(["xcodebuild", "-workspace", "ios/Muxflow.xcworkspace", "-scheme", "Muxflow", "-configuration", "Release", "-sdk", "iphoneos", "-destination", "generic/platform=iOS", "-archivePath", str(archive), "-derivedDataPath", str(REPO / "tmp/work/ios-distribution"), "archive"], cwd=MOBILE, log="archive.log")
            export_options = signing / "ExportOptions.plist"
            export_options.write_bytes(plistlib.dumps({"method": "app-store-connect", "destination": "export", "teamID": team, "signingStyle": "manual", "signingCertificate": "Apple Distribution", "provisioningProfiles": {BUNDLE: profile["UUID"]}, "manageAppVersionAndBuildNumber": False, "uploadSymbols": True}))
            export = OUTPUT / "export"
            run(["xcodebuild", "-exportArchive", "-archivePath", str(archive), "-exportOptionsPlist", str(export_options), "-exportPath", str(export)], log="export.log")
            ipas = list(export.glob("*.ipa"))
            if len(ipas) != 1:
                raise ValueError("Expected exactly one exported IPA")
            unpacked = signing / "unpacked"
            run(["ditto", "-x", "-k", str(ipas[0]), str(unpacked)])
            app = unpacked / "Payload/Muxflow.app"
            run(["codesign", "--verify", "--deep", "--strict", str(app)])
            info = plistlib.loads((app / "Info.plist").read_bytes())
            entitlements = plistlib.loads(run(["codesign", "--display", "--entitlements", "-", "--xml", str(app)], capture=True))
            validate_app(info, entitlements, config, team)
            embedded = plistlib.loads(run(["security", "cms", "-D", "-i", str(app / "embedded.mobileprovision")], capture=True))
            validate_profile(embedded, team)
            ipa = OUTPUT / f'Muxflow-{config["version"]}-{config["ios"]["buildNumber"]}.ipa'
            shutil.copyfile(ipas[0], ipa)
            manifest = {"file": ipa.name, "sha256": hashlib.sha256(ipa.read_bytes()).hexdigest(), "bundleId": BUNDLE, "version": config["version"], "buildNumber": config["ios"]["buildNumber"], "commit": run(["git", "rev-parse", "HEAD"], capture=True).decode().strip(), "teamId": team}
            (OUTPUT / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
            print(json.dumps(manifest), flush=True)
        finally:
            subprocess.run(["security", "list-keychains", "-d", "user", "-s", *original_keychains], stdout=subprocess.DEVNULL)
            if keychain.exists():
                subprocess.run(["security", "delete-keychain", str(keychain)], stdout=subprocess.DEVNULL)
            if installed_profile and installed_profile.exists():
                installed_profile.unlink()


def upload():
    manifest = json.loads((OUTPUT / "manifest.json").read_text())
    ipa = OUTPUT / manifest["file"]
    if hashlib.sha256(ipa.read_bytes()).hexdigest() != manifest["sha256"]:
        raise ValueError("IPA does not match the retained artifact digest")
    key_id = require("MUXFLOW_APP_STORE_CONNECT_KEY_ID")
    issuer = require("MUXFLOW_APP_STORE_CONNECT_ISSUER_ID")
    with tempfile.TemporaryDirectory(prefix="muxflow-asc-", dir=require("RUNNER_TEMP")) as temp:
        key = Path(temp) / f"AuthKey_{key_id}.p8"
        decode_secret("MUXFLOW_APP_STORE_CONNECT_KEY_P8_BASE64", key)
        os.environ["API_PRIVATE_KEYS_DIR"] = temp
        auth = ["--apiKey", key_id, "--apiIssuer", issuer]
        run(["xcrun", "altool", "--validate-app", "-f", str(ipa), "--type", "ios", *auth], log="validate.log")
        run(["xcrun", "altool", "--upload-app", "-f", str(ipa), "--type", "ios", *auth], log="upload.log")
        print(f'Uploaded {manifest["version"]} ({manifest["buildNumber"]}); waiting for the Apple build record.', flush=True)
        # Retain the processed-build identifier when it becomes available.
        # Admission also checks upload history while processing is pending.
        for attempt in range(40):
            builds = find_build(manifest["buildNumber"])
            if builds:
                build_record = builds[0]
                (OUTPUT / "apple-build.json").write_text(json.dumps(build_record, indent=2) + "\n")
                print(f'Apple build {build_record["id"]}: {build_record["attributes"]["processingState"]}; export compliance and device QA remain to be checked.', flush=True)
                return
            time.sleep(15)
        raise ValueError("Upload accepted, but Apple build record is not visible yet; inspect App Store Connect before starting another build")


if __name__ == "__main__":
    os.umask(0o077)
    OUTPUT.mkdir(parents=True, exist_ok=True)
    try:
        if sys.platform != "darwin":
            raise ValueError("Signed iOS builds/uploads require macOS and Xcode")
        {"check": check_number, "build": build, "upload": upload}[sys.argv[1]]()
    except (ValueError, RuntimeError, KeyError, IndexError, OSError) as error:
        print(f"TestFlight: {error}", file=sys.stderr)
        raise SystemExit(1)
