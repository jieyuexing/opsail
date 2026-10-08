"""Atomically exchange two existing sibling directories; never fall back to a gap."""
import ctypes
import os
import sys

left, right = map(os.fsencode, sys.argv[1:])
libc = ctypes.CDLL(None, use_errno=True)
if sys.platform == 'darwin':
    swap = libc.renamex_np
    swap.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
    result = swap(left, right, 2)  # RENAME_SWAP
elif sys.platform.startswith('linux'):
    swap = libc.renameat2
    swap.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    result = swap(-100, left, -100, right, 2)  # AT_FDCWD, RENAME_EXCHANGE
else:
    raise SystemExit('Atomic directory exchange is unavailable on this platform.')
if result:
    raise SystemExit('Atomic directory exchange failed.')
