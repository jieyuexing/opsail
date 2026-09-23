"""Unix PTY acceptance for masked password entry; fictitious secrets only."""
import json
import os
import pathlib
import pty
import select
import signal
import sys
import tempfile
import time

binary = pathlib.Path(sys.argv[1]).resolve()


def session(args, steps):
    pid, fd = pty.fork()
    if pid == 0:
        os.execv(str(binary), [str(binary), *args])
    captured = bytearray()
    deadline = time.monotonic() + 30
    try:
        for marker, value in steps:
            while marker not in captured:
                if time.monotonic() > deadline:
                    raise AssertionError('Timed out waiting for terminal prompt')
                if select.select([fd], [], [], 0.1)[0]:
                    captured.extend(os.read(fd, 65536))
            os.write(fd, value)
        while time.monotonic() < deadline:
            if select.select([fd], [], [], 0.1)[0]:
                try:
                    data = os.read(fd, 65536)
                except OSError:
                    break
                if not data:
                    break
                captured.extend(data)
        else:
            raise AssertionError('Terminal child did not exit')
        _, status = os.waitpid(pid, 0)
        pid = None
        return bytes(captured), os.waitstatus_to_exitcode(status)
    finally:
        os.close(fd)
        if pid is not None:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)


with tempfile.TemporaryDirectory(prefix='opsail-gateway-pty-') as directory:
    # Ctrl-U and Backspace must edit the masked input; confirmation deliberately
    # differs, so neither this check nor cancellation creates a real vault.
    entered = 'discard\x15maskx\x7f中9\n'.encode()
    output, code = session(['gateway', 'init', '--data-dir', directory], [
        (b'Vault passphrase:', entered),
        (b'Confirm vault passphrase:', b'different\n'),
    ])
    assert code == 1
    assert output.count(b'*') >= 8
    assert b'discard' not in output and b'mask' not in output and '中'.encode() not in output
    assert b'passphrase-mismatch' in output
    assert not (pathlib.Path(directory) / 'vault.age').exists()
    output, code = session(['gateway', 'init', '--data-dir', directory], [
        (b'Vault passphrase:', b'never-saved\x03'),
    ])
    assert code != 0 and b'never-saved' not in output
    assert not (pathlib.Path(directory) / 'vault.age').exists()
print(json.dumps({'maskedInput': True, 'editing': True, 'cancellation': True, 'plaintextEcho': False}))
