"""Release gate for pinned OpenCR public-contract observations and route scope."""

from __future__ import annotations

import hashlib
import json
import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
INVENTORY_PATH = ROOT / "docs/qa/opencr-public-operation-coverage.json"


class OpenCrPublicInventoryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.inventory = json.loads(INVENTORY_PATH.read_text(encoding="utf-8"))
        self.source = self.inventory["source"]
        self.spec_path = ROOT / "docs/qa" / self.source["openapi_path"]
        self.spec_bytes = self.spec_path.read_bytes()
        self.spec = json.loads(self.spec_bytes)

    def test_inventory_matches_every_operation_in_pinned_openapi(self) -> None:
        expected = {
            (method.upper(), path, operation.get("operationId"))
            for path, path_item in self.spec["paths"].items()
            for method, operation in path_item.items()
            if method.lower() in {"get", "post", "put", "patch", "delete"}
        }
        listed = {
            (item["method"], item["path"], item["operation_id"])
            for item in self.inventory["operations"]
        }
        self.assertEqual(expected, listed)
        self.assertEqual(len(self.inventory["operations"]), len(listed))

    def test_inventory_records_provenance_and_valid_coverage_dispositions(self) -> None:
        self.assertEqual(self.spec["info"]["version"], self.source["api_version"])
        self.assertEqual(
            self.spec["info"]["license"]["name"], self.source["declared_license"]
        )
        self.assertEqual(
            hashlib.sha256(self.spec_bytes).hexdigest(), self.source["sha256"]
        )
        for item in self.inventory["operations"]:
            with self.subTest(operation=item["operation_id"]):
                self.assertIn(item["coverage"], {"partial", "gap"})
                self.assertTrue(item["limitation"].strip())
                if item["coverage"] == "partial":
                    self.assertIsNotNone(item["litecr_route"])
                else:
                    self.assertIsNone(item["litecr_route"])

    def test_partially_covered_routes_are_registered_with_the_declared_method(self) -> None:
        router_source = (
            ROOT / "crates/api/src/integrations/fhir.rs"
        ).read_text(encoding="utf-8")
        route_lines = [line for line in router_source.splitlines() if ".route(" in line]

        for item in self.inventory["operations"]:
            if item["coverage"] != "partial":
                continue
            with self.subTest(operation=item["operation_id"]):
                route_line = next(
                    (line for line in route_lines if f'"{item["litecr_route"]}"' in line),
                    None,
                )
                self.assertIsNotNone(route_line, "covered route is missing from FHIR router")
                method_token = {"GET": "get(", "POST": ".post("}[item["method"]]
                self.assertRegex(route_line, re.escape(method_token))

    def test_runtime_source_supplement_is_pinned_unprofiled_and_not_claimed(self) -> None:
        supplement = self.inventory["runtime_source_supplement"]
        self.assertIn("not evidence of a deployed route", supplement["scope"])
        self.assertEqual(
            supplement["source"]["repository"],
            "https://github.com/intrahealth/client-registry",
        )
        self.assertEqual(
            supplement["source"]["commit"],
            "7569452055dc6de2719e1e362e78a7d7973856d4",
        )
        self.assertEqual(supplement["source"]["path"], "server/lib/routes/fhir.js")
        self.assertEqual(
            supplement["source"]["url"],
            "https://github.com/intrahealth/client-registry/blob/"
            "7569452055dc6de2719e1e362e78a7d7973856d4/server/lib/routes/fhir.js",
        )
        operations = {
            (item["method"], item["path"], item["observation_id"]): item
            for item in supplement["operations"]
        }
        self.assertEqual(len(operations), len(supplement["operations"]))
        self.assertEqual(
            set(operations),
            {
                ("PUT", "/fhir/{ResourceType}/{id}", "runtime-put-resource"),
                ("GET", "/fhir/Patient/$ihe-pix", "runtime-ihe-pix"),
            },
        )

        router_source = (
            ROOT / "crates/api/src/integrations/fhir.rs"
        ).read_text(encoding="utf-8")
        route_lines = [line for line in router_source.splitlines() if ".route(" in line]
        registered: dict[str, set[str]] = {}
        for line in route_lines:
            route = re.search(r'\.route\("([^"]+)"\s*,(.*)$', line)
            self.assertIsNotNone(route, f"unparsed FHIR route declaration: {line}")
            assert route is not None
            methods = set(re.findall(r"\b(get|post|put|patch|delete)\s*\(", route.group(2)))
            self.assertTrue(methods, f"route declares no supported HTTP method: {line}")
            registered.setdefault(route.group(1), set()).update(methods)

        # Keep this allowlist explicit: new paths and new methods need a deliberate
        # contract-inventory update, including paths that may otherwise look generic.
        self.assertEqual(
            registered,
            {
                "/fhir/metadata": {"get"},
                "/fhir/r4/metadata": {"get"},
                "/fhir/r4/Patient": {"get", "post"},
                "/fhir/r4/Patient/{id}": {"get"},
                "/fhir/Patient": {"get", "post"},
                "/fhir/Patient/{id}": {"get"},
            },
            "FHIR adapter route/method allowlist changed; review the compatibility inventory",
        )

        for item in operations.values():
            with self.subTest(observation=item["observation_id"]):
                self.assertEqual(item["coverage"], "gap")
                self.assertEqual(item["deployment_status"], "unprofiled")
                self.assertIsNone(item["litecr_route"])
                self.assertTrue(item["limitation"].strip())


if __name__ == "__main__":
    unittest.main()
