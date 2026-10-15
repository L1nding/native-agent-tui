"""Component-batch evidence must reject changes beyond the exact approved additions."""

import copy
import json
import unittest

from domain_project_component_geometry import verify_batch_geometry


class ComponentGeometryEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.before = {
            "floors": [{"id": "floor", "walls": [{"id": "wall", "thickness": 0.2}]}]
        }
        self.proposal = {
            "task_id": "task",
            "component_plan": {
                "components": [
                    {
                        "component_id": "bed",
                        "source_instance_id": "source.one",
                        "placement_basis": "source",
                    }
                ]
            },
            "draft": {
                "operations": [
                    {
                        "op": "add",
                        "collection": "solids",
                        "floor_id": "floor",
                        "target_id": "bed.body",
                        "value_json": json.dumps({"id": "bed.body", "x": 1}),
                        "reason": "test",
                        "basis": "assumed",
                        "source_ids": ["source.one"],
                    }
                ]
            },
        }
        self.after = copy.deepcopy(self.before)
        self.after["floors"][0]["solids"] = [
            {
                "id": "bed.body",
                "x": 1,
                "metadata": {
                    "patch_id": "patch.task",
                    "patch_reason": "test",
                    "value_basis": "assumed",
                },
                "source_ids": ["source.one"],
            }
        ]

    def test_exact_addition_to_previously_omitted_collection_is_verified(self):
        result = verify_batch_geometry(
            self.before, self.after, self.proposal, {"source.one"}
        )
        self.assertEqual(result["instances"], 1)
        self.assertNotIn("solids", self.before["floors"][0])

    def test_existing_wall_change_and_unapproved_addition_are_both_rejected(self):
        for mutate in [
            lambda floor: floor["walls"][0].update(thickness=0.1),
            lambda floor: floor["solids"].append({"id": "extra"}),
            lambda floor: floor["solids"][0].update(x=2),
        ]:
            with self.subTest(mutation=mutate):
                changed = copy.deepcopy(self.after)
                mutate(changed["floors"][0])
                with self.assertRaises(AssertionError):
                    verify_batch_geometry(
                        self.before, changed, self.proposal, {"source.one"}
                    )

    def test_unselected_source_or_non_source_pose_cannot_be_approved(self):
        with self.assertRaises(AssertionError):
            verify_batch_geometry(
                self.before, self.after, self.proposal, {"source.other"}
            )
        changed = copy.deepcopy(self.proposal)
        changed["component_plan"]["components"][0]["placement_basis"] = "user_request"
        with self.assertRaises(AssertionError):
            verify_batch_geometry(self.before, self.after, changed, {"source.one"})


if __name__ == "__main__":
    unittest.main()
