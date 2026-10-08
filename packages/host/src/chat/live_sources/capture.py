"""ExternalObservation-compatible capture construction for visible DOM windows.

The shape follows the retired readers' public schema but imports neither their
code nor any retired path.  It is deliberately pure: it has no browser, file,
network, or credential access.
"""

from __future__ import annotations

import hashlib
import json
from urllib.parse import urlsplit, urlunsplit
from datetime import datetime, timezone
from typing import Any


def digest(value: Any) -> str:
    return hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")).hexdigest()


def build_visible_observation(provider: str, conversation_id: str, conversation_name: str, page_url: str, raw_messages: list[dict[str, Any]], limit: int, *, source_mode: str = "live-dom") -> dict[str, Any]:
    observed_at = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    subject_ref = f"chat://{provider}/conversation/{hashlib.sha256(conversation_id.encode()).hexdigest()[:24]}"
    messages = []
    all_media = []
    for index, raw in enumerate(raw_messages[-limit:]):
        text = str(raw.get("text") or "").strip()
        raw_media = raw.get("media") if isinstance(raw.get("media"), list) else []
        if not text and not raw_media:
            continue
        msg_id = str(raw.get("id") or "") or "synthetic-" + hashlib.sha256(f"{conversation_id}\0{raw.get('sender')}\0{raw.get('time')}\0{text}\0{index}".encode()).hexdigest()[:24]
        media_refs, normalized_media = [], []
        for ordinal, media in enumerate(raw_media):
            if not isinstance(media, dict):
                continue
            media_type = str(media.get("type") or "unknown")
            locator = str(media.get("locator") or f"{provider}://message/{msg_id}/media/{ordinal}")
            media_id = hashlib.sha256(f"{subject_ref}|{msg_id}|{ordinal}|{locator}".encode()).hexdigest()
            media_refs.append(media_id)
            normalized_media.append({"media_id": media_id, "parent_message_id": msg_id, "ordinal_in_message": ordinal, "media_type": media_type, "mime_type": None, "source_locator": locator, "locator_digest": hashlib.sha256(locator.encode()).hexdigest(), "capture_status": "locator-only", "blob_digest": None, "byte_size": None, "alt_text": str(media.get("alt_text") or ""), "observed_at": observed_at, "failure_reason": None, "extensions": {}})
        message = {"message_id": msg_id, "revision": 1, "sent_at": str(raw.get("time") or "") or None, "edited_at": None, "sender": {"display_name": str(raw.get("sender") or ""), "ref": None, "is_self": bool(raw.get("is_self"))}, "message_type": "mixed" if text and media_refs else (str(raw_media[0].get("type") or "unknown") if media_refs else "text"), "text": text or "[" + ",".join(str(item.get("type") or "unknown") for item in raw_media if isinstance(item, dict)) + "]", "deleted": False, "reply_to": [], "media_refs": media_refs, "source_anchor": {"provider_ref": f"{provider}://message/{msg_id}", "observed_at": observed_at}, "extensions": {"id_status": "provider" if raw.get("id") else "synthetic", "source_mode": source_mode}}
        message["sender"]["is_self"] = raw.get("is_self") if isinstance(raw.get("is_self"), bool) else None
        message["sender"]["ref"] = raw.get("sender_ref")
        for key in ("raw_time", "date_status", "time_source", "time_zone"):
            if key in raw:
                message["extensions"][key] = raw[key]
        message["content_digest"] = digest(message)
        messages.append(message)
        all_media.extend(normalized_media)
    public_url = urlsplit(page_url)
    public_url = urlunsplit((public_url.scheme, public_url.netloc, public_url.path, "", ""))
    capture = {"schema": 1, "kind": "conversation-capture", "provider_id": provider, "subject_ref": subject_ref, "observed_at": observed_at, "completeness": "visible-window", "sensitivity": "private-communication", "source_window": {"limit": limit, "start": messages[0]["sent_at"] if messages else None, "end": messages[-1]["sent_at"] if messages else None, "boundary_status": "visible-only"}, "messages": messages, "media": all_media, "extensions": {"source_mode": source_mode, "page_url": public_url, "source": {"mode": source_mode, "identity_kind": "visible-dom-name" if provider == "feishu" else "provider-dom", "conversation_id": conversation_id}}}
    capture["capture_id"] = digest({"provider_id": provider, "subject_ref": subject_ref, "observed_at": observed_at, "message_digests": [item["content_digest"] for item in messages]})
    capture["content_digest"] = digest(capture)
    content = {"conversation_id": conversation_id, "conversation_name": conversation_name, "messages": raw_messages[-limit:], "capture": capture}
    return {"schema": 1, "provider_id": provider, "source_kind": "conversation", "subject_ref": subject_ref, "observed_at": observed_at, "completeness": "visible-window", "sensitivity": "private-communication", "delivery": "ephemeral-stdout", "item_count": len(messages), "content_digest": digest(content), "content": content}
