"""Unix socket names shared by the Python clients and native Node host."""
import base64
import re
from pathlib import Path


def runtime_socket_path(root: Path, profile_id: str) -> Path:
    legacy = root / "runtime" / f"{profile_id}.sock"
    if len(str(legacy).encode("utf-8")) <= 103:
        return legacy
    if not re.fullmatch(r"[a-f0-9]{1,32}", profile_id):
        raise ValueError("Opsail profile cannot be encoded as a private Unix socket.")
    # Same lossless leading-nibble encoding as native/common.mjs.
    digits = "1" + profile_id
    if len(digits) % 2:
        digits = "0" + digits
    name = base64.urlsafe_b64encode(bytes.fromhex(digits)).decode("ascii").rstrip("=")
    compact = root / "runtime" / name
    if len(str(compact).encode("utf-8")) > 103:
        raise ValueError("Opsail data root is too long for a private Unix socket.")
    return compact
