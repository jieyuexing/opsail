"""Only synthetic roots and subprocesses; never invoke a chat source."""
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.path.insert(0, str((Path(__file__).resolve().parent / '../../../src').resolve()))
from chat import daily


class DailyLockTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def legacy(self, owner=None):
        path = self.root / 'jobs/daily-collect.lock'
        path.mkdir(parents=True)
        if owner is not None:
            (path / 'owner.json').write_text(json.dumps(owner))
        return path

    def assert_busy(self):
        with self.assertRaises(daily.DailyError) as caught:
            with daily.Lock(self.root):
                self.fail('Entered an occupied lock')
        self.assertEqual(caught.exception.code, 'collect-busy')

    def test_live_legacy_owner_is_not_stolen_after_an_hour(self):
        path = self.legacy({'pid': os.getpid(), 'started_at': time.time() - 7200})
        original = (path / 'owner.json').read_bytes()
        self.assert_busy()
        self.assertEqual((path / 'owner.json').read_bytes(), original)

    def test_legacy_creation_window_is_not_treated_as_a_dead_owner(self):
        path = self.legacy()
        self.assert_busy()
        self.assertTrue(path.is_dir())

    def test_permission_denied_means_owner_may_be_alive(self):
        self.legacy({'pid': 123, 'started_at': time.time()})
        with patch.object(daily.os, 'kill', side_effect=PermissionError):
            self.assert_busy()

    def test_confirmed_dead_legacy_owner_does_not_block(self):
        exited = subprocess.Popen([sys.executable, '-c', 'pass'])
        exited.wait(timeout=5)
        self.legacy({'pid': exited.pid, 'started_at': time.time()})
        with daily.Lock(self.root):
            pass

    def child(self, body):
        code = 'import sys,time\nfrom pathlib import Path\nfrom chat import daily\nroot=Path(sys.argv[1])\n' + body
        env = {**os.environ, 'PYTHONPATH': str(Path(daily.__file__).parent.parent), 'PYTHONDONTWRITEBYTECODE': '1'}
        proc = subprocess.Popen([sys.executable, '-c', code, str(self.root)],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                text=True, env=env)
        def cleanup():
            if proc.poll() is None:
                proc.kill()
            proc.wait(timeout=5)
            for stream in (proc.stdin, proc.stdout, proc.stderr):
                stream.close()
        self.addCleanup(cleanup)
        return proc

    def line(self, proc):
        self.assertTrue(select.select([proc.stdout], [], [], 5)[0], 'child readiness timeout')
        return proc.stdout.readline().strip()

    def test_real_process_mutex_crash_release_and_stable_inode(self):
        holder = self.child("with daily.Lock(root):\n print('held',flush=True)\n sys.stdin.readline()\n")
        self.assertEqual(self.line(holder), 'held')
        self.assert_busy()
        holder.kill()
        holder.wait(timeout=5)
        with daily.Lock(self.root):
            self.assert_busy()
        # An idle lock file is retained; it is not evidence of a busy process.
        path = self.root / 'jobs/daily-collect.flock'
        inode = path.stat().st_ino
        with daily.Lock(self.root):
            self.assertEqual(path.stat().st_ino, inode)

    def test_simultaneous_contenders_never_overlap(self):
        body = """sys.stdin.readline()
for _ in range(30):
 try:
  with daily.Lock(root):
   with (root/'critical').open('x'):
    time.sleep(.003)
   (root/'critical').unlink()
 except daily.DailyError as error:
  assert error.code == 'collect-busy'
 time.sleep(.001)
print('done',flush=True)
"""
        children = [self.child(body) for _ in range(6)]
        for child in children:
            child.stdin.write('\n')
            child.stdin.flush()
        for child in children:
            child.wait(timeout=10)
            self.assertEqual(child.returncode, 0, child.stderr.read())

    def test_error_releases_lock_and_independent_roots_do_not_contend(self):
        with self.assertRaisesRegex(RuntimeError, 'fixture'):
            with daily.Lock(self.root), daily.Lock(self.root / 'another-root'):
                raise RuntimeError('fixture')
        with daily.Lock(self.root):
            pass

    def test_run_cannot_touch_browser_before_acquiring_lock(self):
        rendering = unittest.mock.Mock(return_value='fixture')
        with daily.Lock(self.root):
            with self.assertRaises(daily.DailyError):
                daily.run(self.root, rendering=rendering)
        rendering.assert_not_called()

    def test_run_owns_lock_across_nested_steps_and_snapshot_refresh(self):
        def check_locked(*args):
            self.assert_busy()
            return 'fixture'
        with patch.object(daily, 'collect', return_value={'providers': []}), \
                patch.object(daily, 'sync', return_value={'conversations': []}), \
                patch.object(daily, 'refresh_wechat_snapshot', side_effect=lambda *a: check_locked() and {'result': 'skipped'}), \
                patch.object(daily, 'digest'), patch.object(daily, 'write_index_md'), \
                patch.object(daily, 'chrome_render_flag', return_value=True):
            daily.run(self.root, rendering=check_locked, notify=lambda *a: None)


if __name__ == '__main__':
    unittest.main()
