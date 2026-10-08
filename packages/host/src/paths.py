"""Standalone paths; source and data ownership are configured explicitly."""
import os
from pathlib import Path

PACKAGE_DIR = Path(__file__).resolve().parent.parent
RUNTIME_PACKAGE_DIR = Path(os.environ.get("OPSAIL_RUNTIME_PACKAGE_DIR") or PACKAGE_DIR).resolve()
SOURCE_DIR = Path(os.environ.get("OPSAIL_SOURCE_DIR") or PACKAGE_DIR.parent.parent).resolve()
PIN_PATH = Path(os.environ.get("OPSAIL_PIN_PATH") or RUNTIME_PACKAGE_DIR / "pin.json").resolve()
DATA_HOME = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local/share")
CHAT_DATA_ROOT = Path(os.environ.get("OPSAIL_CHAT_DATA_ROOT") or DATA_HOME / "opsail-host/retained-chat").resolve()
CHAT_BINDING_FILE = CHAT_DATA_ROOT / "bindings.json"
