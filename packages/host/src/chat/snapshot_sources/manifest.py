"""Provenance for clean-room snapshot format compatibility."""

SOURCES = {
    "feishu": {
        "path": "external-observation/bin/feishu-chat",
        "sha256": "d76ea5eac7410abfed555a5cdc1d01c8788e627cb65dec18d310eefc9d1850d9",
        "snapshot_mode": "legacy-local-capture-json",
        "identity_kind": "snapshot-chat-id",
    },
    "teams": {
        "path": "external-observation/bin/teams-chat",
        "sha256": "53fc0b0d6da446d815772e7b52d95909a42a2213ef8e07b9d4495f15e12e4422",
        "snapshot_mode": "local-jsonl-state",
        "identity_kind": "teams-url-conversation-id",
    },
    "wechat": {
        "path": "external-observation/bin/wechat-chat",
        "sha256": "119e2935a0b50ff52158dfd97490045deacc1f51d16a0eb117e8247d38c18c02",
        "snapshot_mode": "local-decrypted-sqlite",
        "identity_kind": "wechat-local-conversation-id",
    },
}
