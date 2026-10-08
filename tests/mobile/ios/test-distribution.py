"""Distribution refusal checks, runnable on Linux with uv."""

import copy
import datetime
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("testflight", Path(__file__).resolve().parents[3] / "release/ios/testflight.py")
distribution = importlib.util.module_from_spec(spec)
spec.loader.exec_module(distribution)
TEAM = "TESTTEAM01"


class DistributionChecks(unittest.TestCase):
    def setUp(self):
        self.profile = {
            "TeamIdentifier": [TEAM], "ExpirationDate": datetime.datetime.now(datetime.UTC).replace(tzinfo=None) + datetime.timedelta(days=1),
            "DeveloperCertificates": [b"certificate"],
            "Entitlements": {"application-identifier": f"{TEAM}.dev.muxflow.mobile", "get-task-allow": False},
        }
        self.config = {"version": "0.1.9", "ios": {"buildNumber": "2"}}
        self.info = {
            "ITSAppUsesNonExemptEncryption": False,
            "CFBundleIdentifier": "dev.muxflow.mobile", "CFBundleShortVersionString": "0.1.9", "CFBundleVersion": "2",
            "NSLocalNetworkUsageDescription": "SSH", "NSMicrophoneUsageDescription": "Voice",
        }
        self.entitlement = {"application-identifier": f"{TEAM}.dev.muxflow.mobile", "com.apple.developer.team-identifier": TEAM, "get-task-allow": False}

    def test_valid_distribution(self):
        distribution.validate_profile(self.profile, TEAM)
        distribution.validate_app(self.info, self.entitlement, self.config, TEAM)

    def test_refuse_an_already_uploaded_number(self):
        with patch.object(distribution, "find_build", return_value=[{"id": "existing-build"}]), self.assertRaises(ValueError):
            distribution.check_number()

    def test_refuse_accepted_upload_before_processing(self):
        with patch.object(distribution, "find_build", return_value=[]), patch.object(distribution, "find_upload", return_value=[{"id": "accepted-upload"}]), self.assertRaises(ValueError):
            distribution.check_number()

    def test_upload_history_query(self):
        with patch.object(distribution, "apple_get", side_effect=[{"data": [{"id": "app"}]}, {"data": []}]) as get:
            self.assertEqual(distribution.find_upload("2"), [])
            self.assertEqual(get.call_args_list[-1].args, ("apps/app/buildUploads", {"filter[cfBundleVersion]": "2", "limit": 1}))

    def test_apple_query_uses_build_number_and_app(self):
        with patch.object(distribution, "apple_get", side_effect=[{"data": [{"id": "app"}]}, {"data": []}]) as get:
            self.assertEqual(distribution.find_build("2"), [])
            self.assertEqual(get.call_args_list[-1].args, ("builds", {"filter[app]": "app", "filter[version]": "2", "limit": 1}))

    def test_refuse_wrong_or_expired_profile(self):
        for changes in ({"TeamIdentifier": ["OTHER"]}, {"ExpirationDate": datetime.datetime(2020, 1, 1)}, {"ProvisionedDevices": ["iphone"]}, {"ProvisionsAllDevices": True}, {"DeveloperCertificates": []}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                distribution.validate_profile({**self.profile, **changes}, TEAM)
        for changes in ({"application-identifier": f"{TEAM}.other"}, {"get-task-allow": True}, {"aps-environment": "production"}):
            profile = copy.deepcopy(self.profile)
            profile["Entitlements"].update(changes)
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                distribution.validate_profile(profile, TEAM)

    def test_refuse_wrong_binary_metadata(self):
        for changes in ({"ITSAppUsesNonExemptEncryption": True}, {"ITSAppUsesNonExemptEncryption": None}, {"CFBundleIdentifier": "other"}, {"CFBundleShortVersionString": "0.1.8"}, {"CFBundleVersion": "1"}, {"UIBackgroundModes": ["remote-notification"]}, {"NSLocalNetworkUsageDescription": ""}, {"NSMicrophoneUsageDescription": ""}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                distribution.validate_app({**self.info, **changes}, self.entitlement, self.config, TEAM)

    def test_refuse_wrong_binary_entitlements(self):
        for changes in ({"application-identifier": f"{TEAM}.other"}, {"com.apple.developer.team-identifier": "OTHER"}, {"get-task-allow": True}, {"aps-environment": "production"}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                distribution.validate_app(self.info, {**self.entitlement, **changes}, self.config, TEAM)


if __name__ == "__main__":
    unittest.main()
