"""TestFlight admission and submission checks; run with uv on Linux."""
import copy
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / 'release/ios'))
import distribute


class TesterDistributionChecks(unittest.TestCase):
    def setUp(self):
        self.build = {"id": "build", "attributes": {"processingState": "VALID", "expired": False, "usesNonExemptEncryption": False, "buildAudienceType": "APP_STORE_ELIGIBLE"}}
        self.group = {"id": "group", "attributes": {"name": "Beta", "isInternalGroup": False}}
        self.detail = {"id": "detail", "attributes": {"externalBuildState": "READY_FOR_BETA_SUBMISSION", "internalBuildState": "READY_FOR_BETA_TESTING"}}
        self.review = {"contactFirstName": "Test", "contactLastName": "Owner", "contactPhone": "123", "contactEmail": "test@example.test", "notes": "Connect to the review host", "demoAccountRequired": False}
        self.app_locales = [{"attributes": {"description": "SSH client", "feedbackEmail": "test@example.test"}}]
        self.build_locales = []
        self.get = patch.object(distribute, "apple_get", side_effect=self.apple_get).start()
        self.write = patch.object(distribute, "apple_request").start()
        patch.object(distribute, "app_id", return_value="app").start()
        self.addCleanup(patch.stopall)

    def apple_get(self, path, params):
        data = {"builds": [self.build], "betaGroups": [self.group], "builds/build/buildBetaDetail": self.detail, "apps/app/betaAppReviewDetail": {"attributes": self.review}, "apps/app/betaAppLocalizations": self.app_locales, "builds/build/betaBuildLocalizations": self.build_locales}
        return {"data": copy.deepcopy(data[path])}

    def external(self):
        distribute.distribute("external", "3", "Beta", "Try terminal scrolling")

    def test_unready_builds_refused_without_writes(self):
        for changes in [{"processingState": "PROCESSING"}, {"processingState": "INVALID"}, {"expired": True}, {"usesNonExemptEncryption": None}, {"usesNonExemptEncryption": True}, {"buildAudienceType": "INTERNAL_ONLY"}]:
            original = copy.deepcopy(self.build)
            self.build["attributes"].update(changes)
            with self.subTest(changes=changes), self.assertRaises(ValueError): self.external()
            self.write.assert_not_called()
            self.build = original

    def test_wrong_audience_or_unknown_group_refused(self):
        self.group["attributes"]["isInternalGroup"] = True
        with self.assertRaises(ValueError): self.external()
        self.group["attributes"]["name"] = "Other"
        with self.assertRaises(ValueError): self.external()
        self.write.assert_not_called()

    def test_review_metadata_required_before_writes(self):
        for missing in ["contactPhone", "notes", "demoAccountRequired"]:
            original = self.review.pop(missing)
            with self.subTest(missing=missing), self.assertRaises(ValueError): self.external()
            self.write.assert_not_called()
            self.review[missing] = original
        self.app_locales = []
        with self.assertRaises(ValueError): self.external()
        self.write.assert_not_called()
        self.app_locales = [{"attributes": {"description": "SSH", "feedbackEmail": "test@example.test"}}, {"attributes": {"description": ""}}]
        with self.assertRaises(ValueError): self.external()
        self.write.assert_not_called()

    def test_demo_credentials_required_when_login_required(self):
        self.review["demoAccountRequired"] = True
        with self.assertRaises(ValueError): self.external()
        self.write.assert_not_called()

    def test_internal_assignment_does_not_submit_beta_review(self):
        self.group["attributes"]["isInternalGroup"] = True
        distribute.distribute("internal", "3", "Beta")
        self.write.assert_called_once_with("POST", "betaGroups/group/relationships/builds", payload={"data": [{"type": "builds", "id": "build"}]})

    def test_external_submits_selected_build_with_notes_and_auto_release(self):
        self.external()
        paths = [call.args[1] for call in self.write.call_args_list]
        self.assertEqual(paths, ["betaBuildLocalizations", "buildBetaDetails/detail", "betaGroups/group/relationships/builds", "betaAppReviewSubmissions"])
        self.assertTrue(self.write.call_args_list[1].kwargs["payload"]["data"]["attributes"]["autoNotifyEnabled"])
        self.assertEqual(self.write.call_args_list[-1].kwargs["payload"]["data"]["relationships"]["build"]["data"]["id"], "build")
        self.assertEqual(self.get.call_args_list[0].args[1]["filter[version]"], "3")

    def test_internal_group_with_all_builds_requires_no_assignment(self):
        self.group["attributes"].update(isInternalGroup=True, hasAccessToAllBuilds=True)
        distribute.distribute("internal", "3", "Beta")
        self.write.assert_not_called()

    def test_pending_review_is_not_submitted_twice(self):
        self.detail["attributes"]["externalBuildState"] = "WAITING_FOR_BETA_REVIEW"
        self.external()
        self.assertNotIn("betaAppReviewSubmissions", [call.args[1] for call in self.write.call_args_list])
        self.assertNotIn("buildBetaNotifications", [call.args[1] for call in self.write.call_args_list])

    def test_approved_build_starts_testing_without_resubmission(self):
        self.detail["attributes"]["externalBuildState"] = "READY_FOR_BETA_TESTING"
        self.external()
        self.assertEqual(self.write.call_args_list[-1].args, ("POST", "buildBetaNotifications"))
        self.assertNotIn("betaAppReviewSubmissions", [call.args[1] for call in self.write.call_args_list])

    def test_existing_notes_preserve_other_locales(self):
        self.build_locales = [{"id": "fr", "attributes": {"locale": "fr-FR"}}, {"id": "en", "attributes": {"locale": "en-US"}}]
        self.external()
        self.assertEqual(self.write.call_args_list[0].args, ("PATCH", "betaBuildLocalizations/en"))


if __name__ == '__main__': unittest.main()
