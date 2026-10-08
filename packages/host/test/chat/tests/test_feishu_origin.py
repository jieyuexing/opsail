"""Exact private-browser origin checks for Feishu tenant bindings."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str((Path(__file__).resolve().parent / '../../../src').resolve()))

from chat.config import ChatError, validate_binding  # noqa: E402
from chat.live import ChatLiveError, _binding_browser  # noqa: E402


def _binding(origin: str, target: str | None = None) -> dict:
    return {
        "browser": {
            "transport": "edge-active-tab",
            "target_url": target or origin + "/next/messenger",
            "allowed_origins": [origin],
        }
    }


class FeishuTenantOriginTests(unittest.TestCase):
    def test_explicit_tenant_origin_is_accepted_by_config_and_live_reader(self):
        binding = _binding("https://fixture-org.feishu.cn")
        configured = validate_binding("feishu", binding, "live-dom")
        self.assertEqual(configured["browser"]["allowed_origins"], ["https://fixture-org.feishu.cn"])
        live = _binding_browser("feishu", binding)
        self.assertEqual(live["allowed_origins"], {"https://fixture-org.feishu.cn"})

    def test_target_must_match_the_explicit_tenant_origin(self):
        binding = _binding("https://fixture-org.feishu.cn", "https://other.feishu.cn/next/messenger")
        with self.assertRaisesRegex(ChatError, "target does not match"):
            validate_binding("feishu", binding, "live-dom")
        with self.assertRaisesRegex(ChatLiveError, "must name one allowed target URL"):
            _binding_browser("feishu", binding)

    def test_rejects_non_origin_or_lookalike_values_in_both_validation_layers(self):
        invalid_origins = (
            "https://fixture-org.feishu.cn/path",
            "https://fixture-org.feishu.cn?query=1",
            "https://fixture-org.feishu.cn#fragment",
            "https://user@fixture-org.feishu.cn",
            "https://fixture-org.feishu.cn:8443",
            "https://fixture-org.feishu.cn.evil.test",
            "https://notfeishu.cn",
            "http://fixture-org.feishu.cn",
        )
        for origin in invalid_origins:
            with self.subTest(origin=origin):
                binding = _binding(origin)
                with self.assertRaises(ChatError):
                    validate_binding("feishu", binding, "live-dom")
                with self.assertRaises(ChatLiveError):
                    _binding_browser("feishu", binding)


if __name__ == "__main__":
    unittest.main()
