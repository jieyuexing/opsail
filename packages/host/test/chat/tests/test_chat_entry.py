"""通过隔离解释器验证统一入口；绝不执行真实 reader 或浏览器操作。"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

PACKAGE = Path(__file__).resolve().parents[3]


class ChatEntryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='chat-entry-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / 'package with spaces'
        shutil.copytree(PACKAGE / 'bin', self.root / 'bin')
        # 模块占位仅供检查路径存在；stub 解释器不会加载任何业务源码。
        source = self.root / 'src/chat'
        source.mkdir(parents=True)
        for name in ('command.js', 'browser.py', 'daily.py'):
            (source / name).touch()
        self.interpreters = Path(self.temp.name) / 'interpreters with spaces'
        self.interpreters.mkdir()
        stub = ('#!' + sys.executable + '\n'
                'import json, os, pathlib, sys\n'
                'print(json.dumps({"interpreter": pathlib.Path(sys.argv[0]).name, "argv": sys.argv[1:]}))\n'
                'sys.exit(int(os.environ.get("CHAT_STUB_EXIT", "0")) if pathlib.Path(sys.argv[1]).is_file() else 97)\n')
        for name in ('node', 'python3', 'custom python'):
            target = self.interpreters / name
            target.write_text(stub)
            target.chmod(0o755)
        self.env = {k: v for k, v in os.environ.items() if k != 'OPSAIL_CHAT_PYTHON'}
        self.env['PATH'] = str(self.interpreters) + os.pathsep + '/usr/bin:/bin'
        # 若实现错误地使用 OPSAIL_NODE，测试应失败；Node 合同是 shell PATH。
        self.env['OPSAIL_NODE'] = '/nonexistent/unused-node'

    def check_route(self, command, module, interpreter, *, exit_code=0):
        args = ['--provider', 'feishu', '--conversation-name', '研发 A 组', '--cursor', 'a b&c', '']
        result = subprocess.run([str(self.root / 'bin/chat'), command, *args],
                                env=self.env, text=True, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, exit_code, result.stderr)
        observed = json.loads(result.stdout)
        self.assertEqual(observed['interpreter'], interpreter)
        self.assertEqual(Path(observed['argv'][0]).resolve(), self.root / 'src/chat' / module)
        self.assertEqual(observed['argv'][1:], [command, *args])

    def test_status_uses_path_node(self):
        self.check_route('status', 'command.js', 'node')

    def test_check_uses_path_node(self):
        self.check_route('check', 'command.js', 'node')

    def test_catalog_uses_path_node(self):
        self.check_route('catalog', 'command.js', 'node')

    def test_read_uses_path_node(self):
        self.check_route('read', 'command.js', 'node')

    def test_browser_uses_path_python_by_default(self):
        for command in ('prepare', 'select'):
            with self.subTest(command=command):
                self.check_route(command, 'browser.py', 'python3')

    def test_daily_commands_use_the_same_python_route(self):
        for command in ('collect', 'sync', 'digest', 'run'):
            with self.subTest(command=command):
                self.check_route(command, 'daily.py', 'python3')
        self.env['OPSAIL_CHAT_PYTHON'] = str(self.interpreters / 'custom python')
        self.env['CHAT_STUB_EXIT'] = '23'
        self.check_route('run', 'daily.py', 'custom python', exit_code=23)

    def test_python_override_preserves_spaces_and_does_not_change_node(self):
        self.env['OPSAIL_CHAT_PYTHON'] = str(self.interpreters / 'custom python')
        for command in ('prepare', 'select'):
            with self.subTest(command=command):
                self.check_route(command, 'browser.py', 'custom python')
        self.check_route('read', 'command.js', 'node')

    def test_empty_python_override_uses_default(self):
        self.env['OPSAIL_CHAT_PYTHON'] = ''
        self.check_route('prepare', 'browser.py', 'python3')

    def test_interpreter_exit_status_is_preserved(self):
        self.env['CHAT_STUB_EXIT'] = '23'
        self.check_route('read', 'command.js', 'node', exit_code=23)
        self.check_route('select', 'browser.py', 'python3', exit_code=23)


if __name__ == '__main__':
    unittest.main()
