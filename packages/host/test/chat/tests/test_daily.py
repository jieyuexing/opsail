import json
import sys
import tempfile
import unittest
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

sys.path.insert(0, str((Path(__file__).resolve().parent / '../../../src').resolve()))
from chat import daily

CST = timezone(timedelta(hours=8))


def row(mid, time, text='hi', *, sender='Ada', ref='u1', me=False, status='known'):
    return {'id': mid, 'time': time, 'date_status': status, 'sender': sender, 'sender_ref': ref, 'is_self': me, 'text': text, 'media': []}


class FakeRunner:
    """Stands in for the existing CLIs; records every invocation."""

    def __init__(self, windows, *, status_ok=True, fail_once=()):
        self.windows, self.status_ok, self.fail_once, self.calls = windows, status_ok, set(fail_once), []

    def __call__(self, args):
        self.calls.append(args)
        name, command = args[0], args[1]
        if command == 'status':
            return 0, {'data': {'providers': [{'modes': {'live-dom': {'available': self.status_ok, 'diagnostic': None if self.status_ok else 'extension-offline'}}}]}}
        if command == 'prepare':
            return 0, None
        if command == 'catalog':
            return 0, {'data': {'entries': [{'conversationId': cid, 'conversationName': f'name-{cid}'} for cid in self.windows]}}
        cid = args[args.index('--conversation-id') + 1]
        if command == 'select':
            if cid in self.fail_once:
                self.fail_once.discard(cid)
                return 2, {'error': {'code': 'message-scope-unverified'}}
            return 0, None
        return 0, {'data': {'content': {'messages': self.windows[cid]}}}


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        (self.root / 'daily.json').write_text(json.dumps({'timezone': 'Asia/Shanghai', 'me': ['示例用户']}))
        self.zone = daily.ZoneInfo('Asia/Shanghai')

    def tearDown(self):
        self.tmp.cleanup()

    def clock(self, *moments):
        values = iter(moments)
        last = [moments[-1]]
        def now():
            try:
                last[0] = next(values)
            except StopIteration:
                pass
            return last[0]
        return now


class DailyTests(Base):
    def test_merge_is_idempotent_keeps_stronger_evidence_and_records_revisions(self):
        folder = self.root / 'c'
        first = [daily.normalize(row('1', '2026-09-23T01:00:00Z', sender='u1'), self.zone)]
        self.assertEqual(daily.merge_messages(folder, first, source='live-dom', observed_at='t1'), (1, 0))
        self.assertEqual(daily.merge_messages(folder, first, source='live-dom', observed_at='t2'), (0, 0))
        # A later window names the sender; a weaker window with no date must not erase the time.
        named = [daily.normalize(row('1', '09:00', sender='Ada', status='unknown'), self.zone)]
        self.assertEqual(daily.merge_messages(folder, named, source='live-dom', observed_at='t3'), (0, 1))
        latest = daily.load_messages(folder)['1']
        self.assertEqual((latest['sender'], latest['date_status'], latest['rev']), ('Ada', 'known', 2))
        self.assertEqual(latest['time'], '2026-09-23T09:00:00+08:00')
        rewrapped = [daily.normalize(row('1', '2026-09-23T01:00:00Z', text='\nhi\n'), self.zone)]
        self.assertEqual(daily.merge_messages(folder, rewrapped, source='sdk-page', observed_at='t3b'), (0, 0))
        edited = [daily.normalize(row('1', '2026-09-23T01:00:00Z', text='edited'), self.zone)]
        daily.merge_messages(folder, edited, source='live-dom', observed_at='t4')
        lines = (folder / 'messages.jsonl').read_text().splitlines()
        self.assertEqual([json.loads(l)['text'] for l in lines], ['hi', 'hi', 'edited'])

    def test_unicode_line_separators_in_text_stay_inside_one_record(self):
        folder = self.root / 'c'
        folder.mkdir()
        text = 'a b c\x85d\x1ce'
        (folder / 'messages.jsonl').write_text(json.dumps({'id': '1', 'text': text}, ensure_ascii=False) + '\n', encoding='utf-8')
        self.assertEqual(daily.load_messages(folder)['1']['text'], text)

    def test_day_status_distinguishes_proven_tail_partial_and_unreadable(self):
        day = date(2026, 9, 23)
        obs = lambda f, t, at: {'from': f, 'to': t, 'observed_at': at, 'count': 3}
        self.assertEqual(daily.day_status([obs('2026-09-22T20:00:00+08:00', '2026-09-24T09:00:00+08:00', '2026-09-24T10:00:00+08:00')], day, self.zone), 'complete')
        self.assertEqual(daily.day_status([obs('2026-09-22T20:00:00+08:00', '2026-09-23T17:00:00+08:00', '2026-09-24T08:00:00+08:00')], day, self.zone), 'likely-complete')
        self.assertEqual(daily.day_status([obs('2026-09-23T11:25:00+08:00', '2026-09-23T16:47:00+08:00', '2026-09-24T08:00:00+08:00')], day, self.zone), 'partial')
        # Two overlapping windows chain into one proven run across the day.
        chained = [obs('2026-09-22T20:00:00+08:00', '2026-09-23T12:00:00+08:00', '2026-09-23T12:05:00+08:00'),
                   obs('2026-09-23T11:00:00+08:00', '2026-09-24T01:00:00+08:00', '2026-09-24T08:00:00+08:00')]
        self.assertEqual(daily.day_status(chained, day, self.zone), 'complete')
        self.assertEqual(daily.day_status([{'from': None, 'to': None, 'observed_at': 'x', 'count': 0}], day, self.zone), 'unreadable')
        self.assertEqual(daily.day_status([], day, self.zone), 'not-observed')
        self.assertEqual(daily.day_status([obs('2026-09-24T09:55:00+08:00', '2026-09-24T10:31:00+08:00', '2026-09-24T12:00:00+08:00')], day, self.zone), 'not-reached')

    def test_collect_selects_each_conversation_retries_once_and_writes_receipt_without_bodies(self):
        windows = {'g1': [row('1', '2026-09-22T12:00:00Z', 'SECRET BODY'), row('2', '2026-09-23T02:00:00Z', '示例用户 请看一下', sender='Bob', ref='u2')],
                   'p1': [row('3', '2026-09-23T03:00:00Z', '这个能改吗？'), row('4', '2026-09-23T03:05:00Z', 'ok', sender='', ref='me', me=True)]}
        runner = FakeRunner(windows, fail_once={'p1'})
        at = datetime(2026, 9, 24, 8, 0, tzinfo=CST)
        receipt = daily.collect(self.root, ['feishu'], runner=runner, now=self.clock(at), sleep=lambda _: None)
        selects = [c for c in runner.calls if c[1] == 'select']
        self.assertEqual(len(selects), 3)
        self.assertEqual([c['added'] for c in receipt['providers'][0]['conversations']], [2, 2])
        receipt_file = next((self.root / 'receipts/collect').rglob('*.json'))
        self.assertNotIn('SECRET BODY', receipt_file.read_text())
        self.assertFalse((self.root / 'jobs/daily-collect.lock').exists())
        index, export = daily.digest(self.root, date(2026, 9, 23))
        cov = {c['conversation_id']: c['status'] for c in index['coverage']}
        self.assertEqual(cov, {'g1': 'likely-complete', 'p1': 'partial'})
        self.assertEqual([(q['excerpt'], q['answered_later']) for q in index['candidates']['questions']], [('这个能改吗？', True)])
        self.assertEqual([m['excerpt'] for m in index['candidates']['mentions']], ['示例用户 请看一下'])
        self.assertEqual(index['candidates']['unreplied'], [])
        self.assertTrue((self.root / 'digest/INDEX.md').exists())
        self.assertEqual([c['name'] for c in export['conversations']], ['name-g1', 'name-p1'])

    def test_blocked_provider_is_reported_and_collects_nothing(self):
        runner = FakeRunner({'g1': []}, status_ok=False)
        receipt = daily.collect(self.root, ['teams'], runner=runner, now=self.clock(datetime(2026, 9, 24, tzinfo=CST)))
        self.assertEqual(receipt['providers'][0]['blocked'], 'extension-offline')
        self.assertFalse(any(c[1] in ('select', 'read') for c in runner.calls))

    def test_live_lock_blocks_a_second_collection(self):
        lock = self.root / 'jobs/daily-collect.lock'
        lock.mkdir(parents=True)
        (lock / 'owner.json').write_text(json.dumps({'pid': __import__('os').getpid(), 'started_at': __import__('time').time()}))
        with self.assertRaises(daily.DailyError):
            daily.collect(self.root, ['feishu'], runner=FakeRunner({}), now=self.clock(datetime(2026, 9, 24, tzinfo=CST)))


def sdk(position, sent_at, text='t', ref='u1', name='Ada', **extra):
    return {'id': f'm{position}', 'chat_id': '100', 'position': position, 'sent_at': sent_at, 'message_type': 4, 'text': text,
            'media': [], 'sender': {'ref': ref, 'display_name': name}, **extra}


class FakePager:
    """Pages newest-first like the extension; the cursor is the next page index."""

    def __init__(self, pages, *, fail_at=None):
        self.pages, self.fail_at, self.calls = pages, fail_at, []

    def __call__(self, cid, cursor):
        index = int(cursor or 0)
        self.calls.append((cid, cursor))
        if index == self.fail_at:
            raise daily.DailyError('selected-chat-page-unrecognized', 'x')
        page = self.pages[index]
        return {'messages': page['messages'], 'invalidPositions': page.get('invalid', []), 'missingPositions': page.get('missing', []),
                'nextCursor': str(index + 1) if index + 1 < len(self.pages) else None}


class ProjectionTests(Base):
    def test_sdk_projection_never_degrades_dom_text_or_media_and_dom_restores_it(self):
        folder = self.root / 'c'
        dom = dict(row('1', '2026-09-23T01:00:00Z', text='第一段 [笑哭]\n第二段', sender='u1'), media=[{'type': 'image'}])
        daily.merge_messages(folder, [daily.normalize(dom, self.zone)], source='live-dom', observed_at='t1')
        sdk_text = dict(row('1', '2026-09-23T01:00:00Z', text='第一段 [Lark_Emoji_Lol_0]第二段', sender='Ada'), media=[{'type': 'unavailable'}])
        self.assertEqual(daily.merge_messages(folder, [daily.normalize(sdk_text, self.zone)], source='sdk-page', observed_at='t2'), (0, 1))
        latest = daily.load_messages(folder)['1']
        self.assertEqual((latest['text'], latest['media'], latest['sender']), ('第一段 [笑哭]\n第二段', [{'type': 'image'}], 'Ada'))
        # A real edit from the SDK still wins; an equivalent DOM rendering then restores readable text.
        daily.merge_messages(folder, [daily.normalize(dict(sdk_text, text='改过了'), self.zone)], source='sdk-page', observed_at='t3')
        self.assertEqual(daily.load_messages(folder)['1']['text'], '改过了')
        daily.merge_messages(folder, [daily.normalize(dict(row('1', '2026-09-23T01:00:00Z', text='改过了'), sender='Ada'), self.zone)], source='live-dom', observed_at='t4')
        self.assertEqual(daily.merge_messages(folder, [daily.normalize(dict(row('1', '2026-09-23T01:00:00Z', text='改过了'), sender='Ada'), self.zone)], source='live-dom', observed_at='t5'), (0, 0))

class SyncTests(Base):
    def at(self):
        return self.clock(datetime(2026, 9, 24, 12, 0, tzinfo=CST))

    def test_pages_back_to_the_date_boundary_and_proves_the_day(self):
        # Seed a DOM-proven self row so the SDK rows can be classified.
        daily.merge_messages(daily.conversation_dir(self.root, 'feishu', '100'), [daily.normalize(row('m8', '2026-09-24T01:00:00Z', sender='me', ref='me', me=True), self.zone)], source='live-dom', observed_at='t')
        daily._write_json(daily.conversation_dir(self.root, 'feishu', '100') / 'conversation.json', {'provider': 'feishu', 'conversation_id': '100', 'name': 'P'})
        pager = FakePager([
            {'messages': [sdk(8, '2026-09-24T01:00:00Z', text='hi', ref='me', name=''), sdk(9, '2026-09-24T02:00:00Z')]},
            {'messages': [sdk(6, '2026-09-23T02:00:00Z', is_recalled=True, text=''), sdk(8, '2026-09-24T01:00:00Z', text='hi', ref='me', name='')], 'invalid': [7]},
            {'messages': [sdk(5, '2026-09-22T10:00:00Z')]},
            {'messages': [sdk(4, '2026-09-21T10:00:00Z')]},
        ])
        receipt = daily.sync(self.root, date(2026, 9, 23), conversation_id='100', runner=FakeRunner({}), pager=pager, now=self.at())
        result = receipt['conversations'][0]
        self.assertEqual((result['pages'], result['reached'], result['added'], result['updated']), (3, True, 3, 0))
        self.assertEqual(len(pager.calls), 3)  # stops once a message older than the day appears
        messages = daily.load_messages(daily.conversation_dir(self.root, 'feishu', '100'))
        self.assertEqual(messages['m6']['state'], 'recalled')
        self.assertIs(messages['m8']['is_self'], True)
        self.assertIs(messages['m9']['is_self'], False)
        observations = json.loads((daily.conversation_dir(self.root, 'feishu', '100') / 'coverage.json').read_text())['observations']
        self.assertEqual(daily.day_status(observations, date(2026, 9, 23), self.zone), 'complete')

    def test_a_missing_position_limits_proven_coverage_and_a_failed_page_keeps_earlier_pages(self):
        pager = FakePager([
            {'messages': [sdk(9, '2026-09-24T02:00:00Z'), sdk(8, '2026-09-23T09:00:00Z')], 'missing': [7]},
            {'messages': [sdk(6, '2026-09-22T10:00:00Z')]},
        ])
        daily.sync(self.root, date(2026, 9, 23), conversation_id='100', runner=FakeRunner({}), pager=pager, now=self.at())
        observations = json.loads((daily.conversation_dir(self.root, 'feishu', '100') / 'coverage.json').read_text())['observations']
        self.assertEqual(observations[-1]['from'], '2026-09-23T17:00:00+08:00')
        self.assertEqual(daily.day_status(observations, date(2026, 9, 23), self.zone), 'partial')
        daily._write_json(daily.conversation_dir(self.root, 'feishu', '200') / 'conversation.json', {'provider': 'feishu', 'conversation_id': '200'})
        failed = daily.sync(self.root, date(2026, 9, 20), conversation_id='200', runner=FakeRunner({}), pager=FakePager([{'messages': [sdk(3, '2026-09-24T02:00:00Z')]}, {}], fail_at=1), now=self.at())
        self.assertEqual((failed['conversations'][0]['error'], failed['conversations'][0]['added']), ('selected-chat-page-unrecognized', 1))
        again = daily.sync(self.root, date(2026, 9, 20), conversation_id='200', runner=FakeRunner({}), pager=FakePager([{'messages': [sdk(3, '2026-09-24T02:00:00Z')]}, {}], fail_at=1), now=self.at())
        self.assertEqual(again['conversations'][0].get('unreadable'), 'selected-chat-page-unrecognized')
        self.assertNotIn('error', again['conversations'][0])

    def test_complete_conversations_are_skipped_and_chat_start_counts_as_boundary(self):
        folder = daily.conversation_dir(self.root, 'feishu', '100')
        daily.add_observation(folder, {'from': '2026-09-20T00:00:00+08:00', 'to': '2026-09-24T09:00:00+08:00', 'observed_at': '2026-09-24T10:00:00+08:00', 'count': 5})
        runner = FakeRunner({'100': [], '300': []})
        pager = FakePager([{'messages': [sdk(2, '2026-09-24T02:00:00Z'), sdk(1, '2026-09-24T01:00:00Z')]}])
        receipt = daily.sync(self.root, date(2026, 9, 22), runner=runner, pager=pager, now=self.at(), sleep=lambda _: None)
        by_id = {r['conversation_id']: r for r in receipt['conversations']}
        self.assertEqual(by_id['100']['skipped'], 'covered')
        self.assertTrue(by_id['300']['reached'])
        observations = json.loads((daily.conversation_dir(self.root, 'feishu', '300') / 'coverage.json').read_text())['observations']
        self.assertEqual(daily.day_status(observations, date(2026, 9, 22), self.zone), 'complete')

class WindowRunner(FakeRunner):
    """Each read returns the next scrolled window of one conversation."""

    def __init__(self, windows):
        super().__init__({'t1': []})
        self.sequence = list(windows)

    def __call__(self, args):
        if args[1] == 'read':
            self.calls.append(args)
            return 0, {'data': {'content': {'messages': self.sequence.pop(0) if len(self.sequence) > 1 else self.sequence[0]}}}
        return super().__call__(args)


class ScrollTests(Base):
    def scroller(self, top_after=None):
        calls = []
        def scroll(provider, cid, direction):
            calls.append(direction)
            ups = calls.count('up')
            return {'moved': not (top_after and ups > top_after), 'atTop': bool(top_after and ups >= top_after)}
        return scroll, calls

    def test_scrolls_up_until_the_day_boundary_then_returns_to_the_bottom(self):
        windows = [[row('m5', '2026-09-23T09:00:00Z'), row('m6', '2026-09-24T01:00:00Z')],
                   [row('m3', '2026-09-23T02:00:00Z'), row('m4', '2026-09-23T05:00:00Z'), row('m5', '2026-09-23T09:00:00Z')],
                   [row('m1', '2026-09-22T10:00:00Z'), row('m2', '2026-09-22T23:00:00Z'), row('m3', '2026-09-23T02:00:00Z')]]
        scroll, calls = self.scroller()
        receipt = daily.sync(self.root, date(2026, 9, 23), provider='teams', conversation_id='19:x@thread.v2', runner=WindowRunner(windows),
                             scroller=scroll, now=self.clock(datetime(2026, 9, 24, 12, 0, tzinfo=CST)), sleep=lambda _: None)
        result = receipt['conversations'][0]
        self.assertEqual((result['steps'], result['reached'], result['added']), (2, True, 6))
        self.assertEqual(calls, ['up', 'up', 'bottom'])
        folder = daily.conversation_dir(self.root, 'teams', '19:x@thread.v2')
        self.assertEqual(folder.name, '19%3Ax%40thread.v2')
        observations = json.loads((folder / 'coverage.json').read_text())['observations']
        self.assertEqual([o.get('tail', True) for o in observations], [True, False, False])
        # Scrolled windows chain into a proven run over 09-23; the first read is the tail.
        self.assertEqual(daily.day_status(observations, date(2026, 9, 23), self.zone), 'complete')

    def test_stale_windows_stop_and_a_scrolled_window_never_claims_the_tail(self):
        same = [row('m5', '2026-09-23T09:00:00Z')]
        scroll, calls = self.scroller()
        result = daily.sync(self.root, date(2026, 9, 20), provider='teams', conversation_id='t1', runner=WindowRunner([same]),
                            scroller=scroll, now=self.clock(datetime(2026, 9, 24, 12, 0, tzinfo=CST)), sleep=lambda _: None)['conversations'][0]
        self.assertFalse(result['reached'])
        self.assertEqual(calls[-1], 'bottom')
        self.assertEqual(calls.count('up'), 3)
        obs = [{'from': '2026-09-22T09:00:00+08:00', 'to': '2026-09-22T12:00:00+08:00', 'observed_at': '2026-09-24T12:00:00+08:00', 'count': 2, 'tail': False}]
        self.assertEqual(daily.day_status(obs, date(2026, 9, 23), self.zone), 'not-observed')

    def test_top_of_conversation_is_a_boundary_and_scroll_failure_still_restores(self):
        windows = [[row('m2', '2026-09-24T01:00:00Z')], [row('m1', '2026-09-24T00:30:00Z'), row('m2', '2026-09-24T01:00:00Z')]]
        scroll, calls = self.scroller(top_after=1)
        result = daily.sync(self.root, date(2026, 9, 20), provider='teams', conversation_id='t1', runner=WindowRunner(windows),
                            scroller=scroll, now=self.clock(datetime(2026, 9, 24, 12, 0, tzinfo=CST)), sleep=lambda _: None)['conversations'][0]
        self.assertTrue(result['reached'])
        observations = json.loads((daily.conversation_dir(self.root, 'teams', 't1') / 'coverage.json').read_text())['observations']
        self.assertEqual(daily.day_status(observations, date(2026, 9, 21), self.zone), 'complete')
        def broken(provider, cid, direction):
            calls.append(direction)
            if direction == 'up':
                raise daily.DailyError('scroll-container-unverified', 'x')
            return {}
        calls.clear()
        failed = daily.sync(self.root, date(2026, 9, 20), provider='teams', conversation_id='t2', runner=WindowRunner(windows),
                            scroller=broken, now=self.clock(datetime(2026, 9, 24, 12, 0, tzinfo=CST)), sleep=lambda _: None)['conversations'][0]
        self.assertEqual((failed['error'], calls), ('scroll-container-unverified', ['up', 'bottom']))

class BrowserStartTests(Base):
    def test_a_closed_chrome_is_started_once_and_waited_for(self):
        states = iter(['extension-unavailable', 'extension-unavailable', 'tab-not-prepared'])
        class Runner:
            def __call__(self, args):
                state = next(states)
                live = {'available': state is None, 'extension': {'reason': state} if state else {}}
                return 0, {'data': {'providers': [{'modes': {'live-dom': live}}]}}
        launched, waits = [], []
        self.assertIsNone(daily.ensure_ready(Runner(), 'teams', launch=lambda: launched.append(1), sleep=waits.append))
        self.assertEqual((len(launched), len(waits)), (1, 2))

    def test_other_blockers_and_a_chrome_that_never_connects_stay_blocked(self):
        class Runner:
            def __init__(self, reason):
                self.reason = reason
            def __call__(self, args):
                return 0, {'data': {'providers': [{'modes': {'live-dom': {'available': False, 'extension': {'reason': self.reason}}}}]}}
        launched = []
        self.assertEqual(daily.ensure_ready(Runner('login-required'), 'feishu', launch=lambda: launched.append(1), sleep=lambda _: None), 'login-required')
        self.assertEqual(launched, [])
        self.assertEqual(daily.ensure_ready(Runner('extension-unavailable'), 'feishu', launch=lambda: launched.append(1), sleep=lambda _: None), 'extension-unavailable')
        self.assertEqual(launched, [1])
    def test_chrome_is_launched_rendering_covered_windows_and_the_running_flag_is_detected(self):
        from unittest.mock import patch
        with patch.object(daily.subprocess, 'run') as run:
            daily.launch_chrome()
        self.assertEqual(run.call_args.args[0], ['open', '-g', '-a', 'Google Chrome', '--args', daily.RENDER_FLAG])
        main = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
        for listing, expected in ((f'{main} {daily.RENDER_FLAG}\n{main} --type=renderer', True), (f'{main}\n{main} --type=gpu {daily.RENDER_FLAG}', False), ('/usr/bin/other', None)):
            with patch.object(daily.subprocess, 'run', return_value=type('R', (), {'stdout': listing})()):
                self.assertIs(daily.chrome_render_flag(), expected)
        self.assertIn('hint', daily._render_hint(False, [{'provider': 'teams', 'errors': ['x']}]))
        self.assertEqual(daily._render_hint(False, [{'provider': 'teams', 'errors': []}]), {})
        self.assertEqual(daily._render_hint(True, [{'provider': 'teams', 'blocked': 'x'}]), {})


class CatalogTests(Base):
    def test_a_rendering_sidebar_is_retried_until_two_reads_agree(self):
        pages = iter([None, ['a'], ['a', 'b'], ['a', 'b']])
        waits = []
        def runner(args):
            ids = next(pages)
            return (2, {'error': {'code': 'live-read-failed'}}) if ids is None else (0, {'data': {'entries': [{'conversationId': i} for i in ids]}})
        entries, failed = daily.stable_catalog(runner, 'feishu', waits.append)
        self.assertEqual(([e['conversationId'] for e in entries], failed, len(waits)), (['a', 'b'], None, 3))
        never = lambda args: (2, {'error': {'code': 'live-read-failed'}})
        self.assertEqual(daily.stable_catalog(never, 'feishu', lambda _: None), (None, 'live-read-failed'))

class ScheduleTests(Base):
    def test_an_unchanged_feed_position_skips_the_click_and_still_proves_the_day(self):
        windows = {'100': [row('1', '2026-09-22T12:00:00Z'), row('2', '2026-09-23T02:00:00Z')]}
        runner = FakeRunner(windows)
        catalog = runner.__call__
        def with_position(args):
            code, envelope = catalog(args)
            if args[1] == 'catalog':
                for entry in envelope['data']['entries']:
                    entry['lastMessagePosition'] = 7
            return code, envelope
        daily.collect(self.root, ['feishu'], runner=with_position, now=self.clock(datetime(2026, 9, 23, 12, 0, tzinfo=CST)), sleep=lambda _: None)
        clicks = sum(1 for c in runner.calls if c[1] == 'select')
        receipt = daily.collect(self.root, ['feishu'], runner=with_position, now=self.clock(datetime(2026, 9, 24, 8, 0, tzinfo=CST)), sleep=lambda _: None)
        self.assertEqual(receipt['providers'][0]['conversations'][0]['skipped'], 'unchanged')
        self.assertEqual(sum(1 for c in runner.calls if c[1] == 'select'), clicks)
        observations = json.loads((daily.conversation_dir(self.root, 'feishu', '100') / 'coverage.json').read_text())['observations']
        self.assertEqual(daily.day_status(observations, date(2026, 9, 23), self.zone), 'complete')

    def test_a_live_pid_is_not_stolen_after_an_hour(self):
        import os, time
        lock = self.root / 'jobs/daily-collect.lock'
        lock.mkdir(parents=True)
        (lock / 'owner.json').write_text(json.dumps({'pid': os.getpid(), 'started_at': time.time() - 4000}))
        with self.assertRaises(daily.DailyError):
            with daily.Lock(self.root):
                self.fail('A live legacy owner must not be preempted')
        self.assertTrue(lock.exists())

    def test_restart_only_a_flagless_chrome_without_user_tabs(self):
        calls = []
        restart = lambda flag, count: daily.ensure_rendering(self.root, flag=lambda: flag, count=lambda targets: count,
                                                             quit=lambda: calls.append('quit') or True, launch=lambda: calls.append('launch'))
        self.assertEqual(restart(None, 0), 'not-running')
        self.assertEqual(restart(True, 0), 'flag-present')
        self.assertEqual(restart(False, 2), 'kept-user-tabs')
        self.assertEqual(restart(False, None), 'kept-user-tabs')
        self.assertEqual(calls, [])
        self.assertEqual(restart(False, 0), 'restarted')
        self.assertEqual(calls, ['quit', 'launch'])

    def test_notifications_follow_the_failure_policy(self):
        sent = []
        notify = lambda title, message: sent.append(message)
        for _ in range(2):
            daily.apply_notify_policy(self.root, {'teams:collect': 'conversation-errors'}, notify)
        self.assertEqual(sent, [])
        daily.apply_notify_policy(self.root, {'teams:collect': 'conversation-errors'}, notify)
        daily.apply_notify_policy(self.root, {'teams:collect': 'conversation-errors'}, notify)
        self.assertEqual(len(sent), 1)  # third run in a row, then not repeated
        daily.apply_notify_policy(self.root, {'feishu:collect': 'login-required'}, notify)
        self.assertEqual(len(sent), 2)  # immediate; the resolved Teams issue is dropped
        daily.apply_notify_policy(self.root, {}, notify)
        daily.apply_notify_policy(self.root, {'feishu:collect': 'login-required'}, notify)
        self.assertEqual(len(sent), 3)  # recurrence after recovery is a new episode

    def test_run_collects_backfills_indexes_and_reports_status(self):
        from unittest.mock import patch
        collected = {'providers': [{'provider': 'feishu', 'conversations': [{'conversation_id': 'c', 'added': 1}]},
                                   {'provider': 'teams', 'blocked': 'login-required', 'conversations': []}]}
        sync_calls = []
        def fake_sync(root, since, provider, runner):
            sync_calls.append((since, provider))
            if provider == 'teams':
                raise daily.DailyError('login-required', 'x')
            return {'conversations': []}
        sent = []
        with patch.object(daily, 'collect', return_value=collected), patch.object(daily, 'sync', side_effect=fake_sync), \
                patch.object(daily, 'chrome_render_flag', return_value=True):
            status = daily.run(self.root, today=date(2026, 9, 24), notify=lambda t, m: sent.append(m), rendering=lambda root: 'flag-present')
        self.assertEqual(sync_calls, [(date(2026, 9, 23), 'feishu'), (date(2026, 9, 23), 'teams')])
        self.assertEqual(status['issues'], {'teams:collect': 'login-required', 'teams:sync': 'login-required'})
        self.assertEqual(len(sent), 2)
        self.assertTrue((self.root / 'digest/2026-09-23/index.json').exists() and (self.root / 'digest/2026-09-24/index.json').exists())
        self.assertIn('最近一次定时运行', (self.root / 'digest/INDEX.md').read_text())

if __name__ == '__main__':
    unittest.main()
