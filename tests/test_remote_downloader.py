import importlib.util
import json
from pathlib import Path
import tempfile
import subprocess
import sys
import fcntl
from concurrent.futures import ThreadPoolExecutor
import threading
import unittest
from http.server import ThreadingHTTPServer
from urllib.error import HTTPError
from urllib.request import Request, urlopen

spec = importlib.util.spec_from_file_location('remote', Path(__file__).parents[1] / 'scripts/remote_downloader.py')
remote = importlib.util.module_from_spec(spec)
spec.loader.exec_module(remote)


class ProtocolTest(unittest.TestCase):
    def test_claim_upload_receipts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            server = ThreadingHTTPServer(('127.0.0.1', 0), remote.handler(root, 'test-token', 0))
            base = f'http://127.0.0.1:{server.server_port}'
            remote.save(root / 'state.json', {'jobs': {'job_1': dict(job_id='job_1', content_id='453c7ddd-9252-4d57-85b9-5310316f8d0d', filetype='wav', status='queued', upload_path='/uploads/test', submit_url=base + '/uploads/test')}, 'events': {}})
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()

            def request(path, body, method='POST', token='test-token', mime='application/json'):
                data = json.dumps(body).encode() if isinstance(body, dict) else body
                headers = {'Content-Type': mime, 'Authorization': 'Bearer ' + token}
                try:
                    with urlopen(Request(base + path, data=data, headers=headers, method=method), timeout=3) as response:
                        return response.status, response.read()
                except HTTPError as error:
                    with error:
                        return error.code, error.read()

            try:
                claim = dict(worker_id='worker', accept=['wav'], max_jobs=1)
                self.assertEqual(request('/jobs/claim', claim, token='wrong')[0], 401)
                self.assertEqual(request('/jobs/claim', claim)[0], 200)
                self.assertEqual(request('/jobs/claim', claim)[0], 204)
                self.assertEqual(request('/uploads/test', b'not audio', 'PUT', mime='audio/wav')[0], 415)
                audio = b'RIFF' + b'\x04\x00\x00\x00' + b'WAVE'
                self.assertEqual(request('/uploads/test', audio, 'PUT', mime='audio/wav')[0], 200)
                self.assertEqual(request('/uploads/test', audio, 'PUT', mime='audio/wav')[0], 409)
                event = dict(worker_id='worker', job_id='job_1', event_id='event_1', status='uploaded')
                self.assertEqual(request('/jobs/claim/events', event)[0], 200)
                self.assertEqual(request('/jobs/claim/events', event)[0], 200)
                self.assertEqual(remote.load(root)['jobs']['job_1']['status'], 'received')
                self.assertEqual((root / 'job_1.wav').read_bytes(), audio)
                self.assertFalse(list(root.glob('*.part')))
            finally:
                server.shutdown()
                server.server_close()
                thread.join()


class MultiDeviceTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), remote.handler(self.root, 'test-token', 0))
        self.base = f'http://127.0.0.1:{self.server.server_port}'
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.directory.cleanup()

    def request(self, path, body):
        try:
            with urlopen(Request(self.base + path, data=json.dumps(body).encode(),
                                 headers={'Authorization': 'Bearer test-token', 'Content-Type': 'application/json'}), timeout=3) as response:
                data = response.read()
                return response.status, json.loads(data) if data else None
        except HTTPError as error:
            with error:
                return error.code, None

    def test_admin_enqueue_status_and_authenticated_file(self):
        code, job = self.request('/admin/jobs', {'content_id': '453c7ddd-9252-4d57-85b9-5310316f8d0d', 'format': 'mp3'})
        self.assertEqual(code, 201)
        job_id = job['job_id']
        self.assertEqual(self.request('/admin/jobs', {'content_id': 'bad'})[0], 400)
        self.assertEqual(self.request('/admin/jobs', {'content_id': '453c7ddd-9252-4d57-85b9-5310316f8d0d', 'format': 'mp4'})[0], 400)

        def get(path, authenticated=True):
            headers = {'Authorization': 'Bearer test-token'} if authenticated else {}
            try:
                with urlopen(Request(self.base + path, headers=headers), timeout=3) as response:
                    return response.status, response.read()
            except HTTPError as error:
                with error:
                    return error.code, error.read()

        self.assertEqual(get('/admin/health', False)[0], 401)
        self.assertEqual(json.loads(get('/admin/health')[1])['protocol'], 1)
        endpoint = '/admin/jobs/' + job_id
        self.assertEqual(get(endpoint, False)[0], 401)
        code, data = get(endpoint)
        self.assertEqual(code, 200)
        self.assertNotIn(b'upload', data)
        self.assertNotIn(b'test-token', data)
        self.assertEqual(get(endpoint + '/file')[0], 409)
        _, claim = self.claim('admin-test')
        audio = b'ID3test-audio'
        with urlopen(Request(claim['submit_url'], data=audio, method='PUT', headers={'Content-Type': 'audio/mpeg'}), timeout=3) as response:
            self.assertEqual(response.status, 200)
        code, data = get(endpoint)
        status = json.loads(data)
        self.assertEqual(status['sha256'], remote.hashlib.sha256(audio).hexdigest())
        self.assertEqual(status['bytes'], len(audio))
        self.assertNotIn(b'uploads/', data)
        self.assertNotIn(b'file"', data)
        self.assertEqual(get(endpoint + '/file'), (200, audio))
        self.assertEqual(get(endpoint + '/file', False)[0], 401)
        self.assertEqual(get(endpoint + '/../../state.json')[0], 404)

    def claim(self, device, **changes):
        body = dict(worker_id='worker_' + device, device_id=device, accept=['mp3', 'wav'],
                    accept_types=['download', 'probe'], max_jobs=1)
        body.update(changes)
        return self.request('/jobs/claim', body)

    def event(self, job, device, status, event_id='event', **changes):
        body = dict(worker_id='worker_' + device, device_id=device, type=job['type'],
                    job_id=job['job_id'], status=status, event_id=event_id)
        body.update(changes)
        return self.request('/jobs/claim/events', body)[0]

    def store(self, state):
        remote.save(self.root / 'state.json', state)

    def test_two_devices_targeted_probes_and_receipt_idempotency(self):
        state = remote.load(self.root)
        a = remote.new_job(state, 0, device_id='a', kind='probe')
        b = remote.new_job(state, 0, device_id='b', kind='probe')
        self.store(state)
        self.assertEqual(self.claim('c')[0], 204)
        self.assertEqual(self.claim('a', accept_types=['download'])[0], 204)
        code, payload = self.claim('b')
        self.assertEqual(code, 200)
        self.assertEqual(payload, dict(job_id=b['job_id'], type='probe', device_id='b'))
        self.assertEqual(self.event(b, 'a', 'probed'), 409)
        self.assertEqual(self.event(b, 'b', 'uploaded'), 400)
        self.assertEqual(self.event(b, 'b', 'probed', type='download'), 409)
        self.assertEqual(self.event(b, 'b', 'probed'), 200)
        self.assertEqual(self.event(b, 'b', 'probed'), 200)
        self.assertEqual(self.event(b, 'b', 'failed'), 409)  # reused event ID
        self.assertEqual(self.claim('a')[1]['job_id'], a['job_id'])
        view = remote.status_view(remote.load(self.root))
        devices = {d['device_id']: d for d in view['devices']}
        self.assertIn('last_probed', devices['b'])
        self.assertNotIn('last_probed', devices['a'])
        self.assertEqual(devices['a']['activity'], 'busy_lease')
        self.assertEqual(devices['b']['activity'], 'available')
        self.assertEqual(self.claim('a', worker_id='other')[0], 409)
        self.assertEqual(self.event(a, 'a', 'probed', 'a-event'), 200)
        self.assertEqual(self.claim('a')[0], 204)

    def test_download_routing_capabilities_and_sanitized_status(self):
        state = remote.load(self.root)
        job = remote.new_job(state, 0, '453c7ddd-9252-4d57-85b9-5310316f8d0d', 'wav', 'a')
        self.store(state)
        self.assertEqual(self.claim('b')[0], 204)
        self.assertEqual(self.claim('a', accept=['mp3'])[0], 204)
        self.assertEqual(self.claim('a', accept_types=['probe'])[0], 204)
        code, payload = self.claim('a')
        self.assertEqual(code, 200)
        self.assertEqual(payload['type'], 'download')
        self.assertEqual(payload['device_id'], 'a')
        self.assertEqual(self.event(job, 'a', 'probed'), 400)
        self.assertEqual(self.event(job, 'a', 'uploaded'), 409)
        self.assertEqual(self.event(job, 'a', 'started'), 200)
        self.assertEqual(self.event(job, 'a', 'outcome_unknown', 'unknown'), 200)
        self.assertEqual(self.event(job, 'a', 'started', 'late'), 409)
        view = json.dumps(remote.status_view(remote.load(self.root)))
        self.assertNotIn('submit_url', view)
        self.assertNotIn('upload_path', view)
        self.assertNotIn('/uploads/', view)

    def test_legacy_worker_cannot_claim_probe(self):
        state = remote.load(self.root)
        remote.new_job(state, 0, kind='probe')
        job = remote.new_job(state, 0, '453c7ddd-9252-4d57-85b9-5310316f8d0d')
        self.store(state)
        code, payload = self.request('/jobs/claim', dict(worker_id='legacy', accept=['mp3'], max_jobs=1))
        self.assertEqual(code, 200)
        self.assertEqual(payload['job_id'], job['job_id'])
        self.assertEqual(payload['device_id'], 'legacy')

    def test_expired_lease_unknown_never_reassigned_or_reclaimed(self):
        for status in ('claimed', 'started', 'processing', 'uploading', 'outcome_unknown', 'unknown'):
            with self.subTest(status=status):
                state = remote.load(self.root)
                state['jobs'] = {}
                job = remote.new_job(state, 0, '453c7ddd-9252-4d57-85b9-5310316f8d0d', device_id='a')
                job.update(status=status, worker_id='worker_a', lease_expires_at='2000-01-01T00:00:00Z')
                waiting = remote.new_job(state, 0, '453c7ddd-9252-4d57-85b9-5310316f8d0d', device_id='a')
                self.store(state)
                self.assertEqual(self.claim('a')[0], 204)
                self.assertEqual(self.claim('a-renamed', worker_id='worker_a')[0], 204)
                current = remote.load(self.root)
                self.assertEqual(remote.status_view(current)['devices'][0]['activity'], 'reconciliation_required')
                with self.assertRaisesRegex(ValueError, 'reconcile'):
                    remote.reassign(current, job['job_id'], 'b')
                remote.distribute(current)
                self.assertEqual(current['jobs'][job['job_id']]['device_id'], 'a')
                self.assertEqual(current['jobs'][waiting['job_id']]['status'], 'queued')

    def test_distribution_and_explicit_queued_reassignment(self):
        self.assertEqual(self.claim('a')[0], 204)
        self.assertEqual(self.claim('b')[0], 204)
        state = remote.load(self.root)
        remote.register(state, 'never-polled')
        jobs = [remote.new_job(state, 0, '453c7ddd-9252-4d57-85b9-5310316f8d0d') for _ in range(4)]
        probe = remote.new_job(state, 0, device_id='b', kind='probe')
        assignments = remote.distribute(state)
        self.assertEqual([a['device_id'] for a in assignments], ['a', 'b', 'a', 'b'])
        self.assertEqual(probe['device_id'], 'b')
        self.assertEqual(remote.distribute(state), assignments)
        remote.reassign(state, jobs[0]['job_id'], 'b')
        self.assertEqual(jobs[0]['device_id'], 'b')
        old = remote.utcnow() - remote.timedelta(seconds=121)
        state['devices']['a']['last_seen'] = remote.stamp(old)
        state['devices']['a']['last_probed'] = remote.stamp(remote.utcnow())
        view = {d['device_id']: d for d in remote.status_view(state)['devices']}
        self.assertEqual(view['a']['activity'], 'offline_inferred')
        self.assertEqual(view['never-polled']['activity'], 'offline_inferred')
        remote.distribute(state)
        self.assertTrue(all(j['device_id'] == 'b' for j in jobs))

    def test_atomic_claim_contention_and_restart_preserves_ownership(self):
        state = remote.load(self.root)
        job = remote.new_job(state, 0, '453c7ddd-9252-4d57-85b9-5310316f8d0d')
        self.store(state)
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(self.claim, ['a', 'b']))
        self.assertEqual(sorted(code for code, _ in results), [200, 204])
        saved = remote.load(self.root)
        claimed = saved['jobs'][job['job_id']]
        owner = claimed['device_id']
        self.assertEqual(self.claim(owner)[0], 204)
        with self.assertRaises(ValueError):
            remote.reassign(saved, job['job_id'], 'other')
        # Recreating the handler does not reset state or make a claim retryable.
        remote.handler(self.root, 'test-token', 0)
        self.assertEqual(remote.load(self.root)['jobs'][job['job_id']], claimed)

    def test_bad_inputs(self):
        for changes in ({'device_id': '../bad'}, {'accept_types': 'probe'}, {'accept': None},
                        {'accept_types': ['unknown']}, {'max_jobs': 2}):
            with self.subTest(changes=changes):
                self.assertEqual(self.claim('a', **changes)[0], 400)
        self.assertEqual(self.request('/jobs/claim', [1])[0], 400)


class OfflineCliTest(unittest.TestCase):
    def run_cli(self, root, *args):
        return subprocess.run([sys.executable, str(Path(__file__).parents[1] / 'scripts/remote_downloader.py'),
                               '--state-dir', str(root), *args], capture_output=True, text=True)

    def test_stopped_service_lock_and_cli_commands(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with (root / 'server.lock').open('a') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                for args in [('enqueue', '453c7ddd-9252-4d57-85b9-5310316f8d0d'),
                             ('probe', '--device', 'a'), ('register', 'a'),
                             ('reassign', 'missing', '--device', 'a'), ('distribute',)]:
                    result = self.run_cli(root, *args)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn('state is locked', result.stderr)
                self.assertFalse((root / 'state.json').exists())
                self.assertEqual(self.run_cli(root, 'status').returncode, 0)
            self.assertEqual(self.run_cli(root, 'register', 'a').returncode, 0)
            result = self.run_cli(root, 'probe', '--device', 'a')
            self.assertEqual(result.returncode, 0, result.stderr)
            first = json.loads(result.stdout)['job_id']
            second = json.loads(self.run_cli(root, 'probe', '--device', 'a').stdout)['job_id']
            self.assertNotEqual(first, second)
            result = self.run_cli(root, 'reassign', first, '--device', 'b')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(remote.load(root)['jobs'][first]['device_id'], 'b')
            self.assertEqual(self.run_cli(root, 'distribute').returncode, 0)
            result = self.run_cli(root, 'enqueue', '453c7ddd-9252-4d57-85b9-5310316f8d0d', '--device', 'b')
            self.assertEqual(result.returncode, 0, result.stderr)
            state = remote.load(root)
            state['jobs'][first].update(status='claimed', worker_id='worker_b')
            remote.save(root / 'state.json', state)
            before = (root / 'state.json').read_bytes()
            result = self.run_cli(root, 'reassign', first, '--device', 'a')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('reconcile', result.stderr)
            self.assertEqual((root / 'state.json').read_bytes(), before)


if __name__ == '__main__':
    unittest.main()
