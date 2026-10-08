# /// script
# dependencies = ["pyjwt[crypto]==2.10.1"]
# ///
"""Assign an existing processed build to a TestFlight group; never rebuild it."""

import argparse
import json
import os
import sys

from testflight import MOBILE, apple_get, apple_request, app_id


def distribute(audience, number, group_name, notes=""):
    if not group_name:
        raise ValueError("Configure the TestFlight group name in the ios-testflight environment")
    app = app_id()
    builds = apple_get("builds", {"filter[app]": app, "filter[version]": number, "limit": 2})["data"]
    if len(builds) != 1:
        raise ValueError("Expected one uploaded build for this app/build number")
    build = builds[0]
    attributes = build["attributes"]
    if attributes["processingState"] != "VALID" or attributes.get("expired"):
        raise ValueError("Build must finish processing and must not be expired")
    if attributes.get("usesNonExemptEncryption") is not False:
        raise ValueError("Complete export compliance before distributing this build")
    groups = apple_get("betaGroups", {"filter[app]": app, "filter[name]": group_name, "limit": 200})["data"]
    groups = [group for group in groups if group["attributes"]["name"] == group_name]
    if len(groups) != 1 or groups[0]["attributes"]["isInternalGroup"] != (audience == "internal"):
        raise ValueError("Expected one group belonging to this app and the selected audience")
    group = groups[0]
    build_id = build["id"]
    detail = apple_get(f"builds/{build_id}/buildBetaDetail", {})["data"]
    state = detail["attributes"]["internalBuildState" if audience == "internal" else "externalBuildState"]
    if audience == "internal":
        if state not in {"READY_FOR_BETA_TESTING", "IN_BETA_TESTING"}:
            raise ValueError(f"Build is not ready for internal testing: {state}")
    else:
        if attributes.get("buildAudienceType") == "INTERNAL_ONLY":
            raise ValueError("An internal-only upload cannot be distributed externally")
        if state not in {"READY_FOR_BETA_SUBMISSION", "WAITING_FOR_BETA_REVIEW", "IN_BETA_REVIEW", "BETA_APPROVED", "READY_FOR_BETA_TESTING", "IN_BETA_TESTING"}:
            raise ValueError(f"Build cannot be submitted or distributed externally: {state}")
        if not notes.strip():
            raise ValueError("Provide What to Test notes for the external build")
        review = apple_get(f"apps/{app}/betaAppReviewDetail", {})["data"]["attributes"]
        required = ["contactFirstName", "contactLastName", "contactPhone", "contactEmail", "notes"]
        if review.get("demoAccountRequired") is True:
            required += ["demoAccountName", "demoAccountPassword"]
        if any(not review.get(name) for name in required) or review.get("demoAccountRequired") is None:
            raise ValueError("Complete TestFlight review contact, sign-in choice and SSH testing instructions in App Store Connect")
        localizations = apple_get(f"apps/{app}/betaAppLocalizations", {"limit": 200})["data"]
        if not localizations or any(not item["attributes"].get("description") for item in localizations) or not any(item["attributes"].get("feedbackEmail") for item in localizations):
            raise ValueError("Complete TestFlight description and feedback email in App Store Connect")
        localized = apple_get(f"builds/{build_id}/betaBuildLocalizations", {"limit": 200})["data"]
        localized = [item for item in localized if item["attributes"]["locale"] == "en-US"]
        if localized:
            apple_request("PATCH", f'betaBuildLocalizations/{localized[0]["id"]}', payload={"data": {"type": "betaBuildLocalizations", "id": localized[0]["id"], "attributes": {"whatsNew": notes}}})
        else:
            apple_request("POST", "betaBuildLocalizations", payload={"data": {"type": "betaBuildLocalizations", "attributes": {"locale": "en-US", "whatsNew": notes}, "relationships": {"build": {"data": {"type": "builds", "id": build_id}}}}})
        apple_request("PATCH", f'buildBetaDetails/{detail["id"]}', payload={"data": {"type": "buildBetaDetails", "id": detail["id"], "attributes": {"autoNotifyEnabled": True}}})
    # Additive relationship POST preserves older builds and is safe to repeat.
    if not (audience == "internal" and group["attributes"].get("hasAccessToAllBuilds")):
        apple_request("POST", f'betaGroups/{group["id"]}/relationships/builds', payload={"data": [{"type": "builds", "id": build_id}]})
    if audience == "external" and state == "READY_FOR_BETA_SUBMISSION":
        apple_request("POST", "betaAppReviewSubmissions", payload={"data": {"type": "betaAppReviewSubmissions", "relationships": {"build": {"data": {"type": "builds", "id": build_id}}}}})
        print(f"Build {number} submitted to Apple's beta review for {group_name}; testers receive it after approval.")
    elif audience == "external" and state in {"WAITING_FOR_BETA_REVIEW", "IN_BETA_REVIEW"}:
        print(f"Build {number} assigned to {group_name}; Apple's beta review is still pending.")
    else:
        if audience == "external" and state in {"BETA_APPROVED", "READY_FOR_BETA_TESTING"}:
            apple_request("POST", "buildBetaNotifications", payload={"data": {"type": "buildBetaNotifications", "relationships": {"build": {"data": {"type": "builds", "id": build_id}}}}})
        print(f"Build {number} available to {group_name} ({audience}).")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("audience", choices=["internal", "external"])
    parser.add_argument("--build-number", default=json.loads((MOBILE / "app.json").read_text())["expo"]["ios"]["buildNumber"])
    args = parser.parse_args()
    try:
        distribute(args.audience, args.build_number, os.environ.get("TESTFLIGHT_GROUP", ""), os.environ.get("TESTFLIGHT_NOTES", ""))
    except (ValueError, RuntimeError, KeyError, OSError) as error:
        print(f"TestFlight distribution: {error}", file=sys.stderr)
        sys.exit(1)
