#!/usr/bin/env python3
"""Local GSuno assignment server. No provider credentials are needed."""
import argparse
import fcntl
import hashlib
import hmac
import json
import os
import re
from pathlib import Path
import secrets
import threading
import uuid
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LIMIT = 256 * 1024 * 1024
LOCK = threading.Lock()


def save(path, data):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    temporary = path.with_suffix('.tmp')
    with open(temporary, 'w', opener=lambda p, f: os.open(p, f, 0o600)) as stream:
        json.dump(data, stream, indent=2)
    temporary.replace(path)


def load(root):
    path = root / 'state.json'
    state = json.loads(path.read_text()) if path.exists() else {'jobs': {}, 'events': {}}
    state.setdefault('devices', {})
    return state


# Recent polling is evidence of connectivity, not proof of browser/provider health.
POLL_WINDOW = 120
TERMINAL = {'received', 'probed', 'failed', 'cancelled'}


def utcnow():
    return datetime.now(timezone.utc)


def stamp(now):
    return now.isoformat(timespec='seconds').replace('+00:00', 'Z')


def timestamp(value):
    try:
        result = datetime.fromisoformat(value.replace('Z', '+00:00'))
        return result if result.tzinfo is not None else None
    except (ValueError, TypeError, AttributeError):
        return None


def identity(value):
    if not isinstance(value, str) or not re.fullmatch(r'[A-Za-z0-9_-]{1,128}', value):
        raise ValueError('invalid device identifier')
    return value


def assigned_jobs(state, device_id):
    return [j for j in state['jobs'].values()
            if j.get('device_id', j.get('worker_id')) == device_id]


def unresolved(job):
    # Expiry is NOT evidence that a provider operation or PUT did not happen.
    return job['status'] != 'queued' and job['status'] not in TERMINAL


def activity(state, device, now):
    jobs = assigned_jobs(state, device['device_id'])
    active = [j for j in state['jobs'].values() if unresolved(j)
              and (j.get('device_id', j.get('worker_id')) == device['device_id']
                   or (device.get('worker_id') is not None
                       and j.get('worker_id') == device['worker_id']))]
    seen = timestamp(device.get('last_seen'))
    recent = seen is not None and 0 <= (now - seen).total_seconds() <= POLL_WINDOW
    live_leases = [j for j in active if (timestamp(j.get('lease_expires_at')) or now) > now]
    return dict(device, recent_poll=recent,
                activity=('busy_lease' if live_leases else 'reconciliation_required' if active
                          else 'available' if recent else 'offline_inferred'),
                assigned_jobs=[j['job_id'] for j in jobs],
                active_jobs=[j['job_id'] for j in active])


def status_view(state, now=None):
    now = now or utcnow()
    return {'jobs': [{k: v for k, v in j.items() if k not in ('upload_path', 'submit_url', 'submit_headers')}
                     for j in state['jobs'].values()],
            'devices': [activity(state, d, now) for d in state['devices'].values()],
            'poll_window_seconds': POLL_WINDOW}


def register(state, device_id):
    device_id = identity(device_id)
    return state['devices'].setdefault(device_id, {'device_id': device_id})


def new_job(state, port, content_id=None, filetype='mp3', device_id=None, kind='download'):
    if device_id is not None:
        register(state, device_id)
    job_id = ('probe_' if kind == 'probe' else 'job_') + uuid.uuid4().hex
    job = dict(job_id=job_id, type=kind, status='queued')
    if device_id is not None:
        job['device_id'] = device_id
    if kind == 'download':
        path = '/uploads/' + secrets.token_urlsafe(32)
        job.update(content_id=str(content_id), filetype=filetype, upload_path=path,
                   submit_url=f'http://127.0.0.1:{port}{path}')
    state['jobs'][job_id] = job
    return job


def reassign(state, job_id, device_id):
    job = state['jobs'].get(job_id)
    if not job or job['status'] != 'queued':
        raise ValueError('only queued jobs may be reassigned; reconcile claimed/processing/uploading/unknown outcomes first (lease expiry is not safe retry)')
    register(state, device_id)
    job['device_id'] = device_id
    return job


def distribute(state, now=None):
    """Explicitly balance queued downloads over recent, idle pollers only."""
    now = now or utcnow()
    devices = [d for d in state['devices'].values()
               if activity(state, d, now)['activity'] == 'available'
               and 'download' in d.get('accept_types', [])]
    counts = {d['device_id']: 0 for d in devices}
    assignments = []
    for job in state['jobs'].values():
        if job['status'] != 'queued' or job.get('type', 'download') != 'download':
            continue
        candidates = [d for d in devices if job['filetype'] in d.get('accept', [])]
        if not candidates:
            continue
        device = min(candidates, key=lambda d: (counts[d['device_id']], d['device_id']))
        reassign(state, job['job_id'], device['device_id'])
        counts[device['device_id']] += 1
        assignments.append({'job_id': job['job_id'], 'device_id': device['device_id']})
    return assignments


def valid_audio(data, kind):
    if kind == 'wav':
        return len(data) >= 12 and data[:4] == b'RIFF' and data[8:12] == b'WAVE'
    return data[:3] == b'ID3' or (len(data) >= 2 and data[0] == 255 and data[1] & 0xE6 == 0xE2)


def handler(root, token, port):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def reply(self, code, data=None):
            payload = json.dumps(data).encode() if data is not None else b''
            self.send_response(code)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def body(self, limit):
            if self.headers.get('Transfer-Encoding'):
                raise ValueError('transfer encoding unsupported')
            size = int(self.headers.get('Content-Length', '-1'))
            if not 0 <= size <= limit:
                raise ValueError('invalid body size')
            data = self.rfile.read(size)
            if len(data) != size:
                raise ValueError('incomplete body')
            return data

        def authorized(self):
            return hmac.compare_digest(self.headers.get('Authorization', ''), 'Bearer ' + token)

        def do_GET(self):
            self.connection.settimeout(30)
            if not self.authorized():
                return self.reply(401)
            if self.path == '/admin/health':
                return self.reply(200, {'protocol': 1, 'formats': ['mp3', 'wav']})
            match = re.fullmatch(r'/admin/jobs/(job_[a-f0-9]{32})(/file)?', self.path)
            if not match:
                return self.reply(404)
            with LOCK:
                job = load(root)['jobs'].get(match[1])
                if not job:
                    return self.reply(404)
                if not match[2]:
                    keys = ('job_id', 'content_id', 'filetype', 'status', 'bytes', 'sha256', 'receipt_status')
                    return self.reply(200, {k: job[k] for k in keys if k in job})
                if job['status'] != 'received' or job.get('filetype') not in ('mp3', 'wav'):
                    return self.reply(409)
                # Never follow a path supplied in persisted state or by the caller.
                path = root / (job['job_id'] + '.' + job['filetype'])
                if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= LIMIT:
                    return self.reply(409)
                self.send_response(200)
                self.send_header('Content-Type', 'audio/mpeg' if job['filetype'] == 'mp3' else 'audio/wav')
                self.send_header('Content-Length', str(path.stat().st_size))
                self.end_headers()
            try:
                with path.open('rb') as stream:
                    while chunk := stream.read(65536):
                        self.wfile.write(chunk)
            except OSError:
                self.close_connection = True

        def do_POST(self):
            self.connection.settimeout(30)
            if not hmac.compare_digest(self.headers.get('Authorization', ''), 'Bearer ' + token):
                return self.reply(401)
            try:
                body = json.loads(self.body(32768))
                if not isinstance(body, dict):
                    raise ValueError('invalid body')
                if self.path == '/admin/jobs':
                    content_id = str(uuid.UUID(body['content_id']))
                    filetype = body.get('format', 'mp3')
                    if filetype not in ('mp3', 'wav'):
                        return self.reply(400)
                    with LOCK:
                        state = load(root)
                        job = new_job(state, self.server.server_port, content_id, filetype)
                        save(root / 'state.json', state)
                        return self.reply(201, {'job_id': job['job_id'], 'status': job['status']})
                worker = body['worker_id']
                if not isinstance(worker, str) or not 1 <= len(worker) <= 128:
                    raise ValueError('invalid worker')
                # Older workers omit routing fields and only understand downloads.
                device_id = identity(body.get('device_id', worker))
                with LOCK:
                    state = load(root)
                    if self.path == '/jobs/claim':
                        accept = body.get('accept')
                        types = body.get('accept_types', ['download'])
                        if (body.get('max_jobs') != 1 or not isinstance(accept, list)
                                or any(t not in ('mp3', 'wav') for t in accept)
                                or not isinstance(types, list)
                                or any(t not in ('download', 'probe') for t in types)):
                            return self.reply(400)
                        device = register(state, device_id)
                        if device.get('worker_id') not in (None, worker):
                            # Device names must be unique per installation.
                            return self.reply(409)
                        now = utcnow()
                        device.update(worker_id=worker, last_seen=stamp(now), accept=accept, accept_types=types)
                        # Never grant another job to a device with an unresolved claim,
                        # even after expiry or an outcome_unknown receipt.
                        if not any(unresolved(j) and (j.get('device_id', j.get('worker_id')) == device_id
                                                      or j.get('worker_id') == worker)
                                   for j in state['jobs'].values()):
                            for job in state['jobs'].values():
                                kind = job.get('type', 'download')
                                if (job['status'] != 'queued' or kind not in types
                                        or job.get('device_id', device_id) != device_id
                                        or (kind == 'download' and job['filetype'] not in accept)):
                                    continue
                                job.update(status='claimed', worker_id=worker, device_id=device_id,
                                           lease_expires_at=stamp(now + timedelta(minutes=30)))
                                save(root / 'state.json', state)
                                keys = (('job_id',) if kind == 'probe' else
                                        ('job_id', 'content_id', 'filetype', 'submit_url', 'lease_expires_at'))
                                return self.reply(200, dict({key: job[key] for key in keys}, type=kind, device_id=device_id))
                        save(root / 'state.json', state)
                        return self.reply(204)
                    if self.path != '/jobs/claim/events':
                        return self.reply(404)
                    event = body['event_id']
                    if not isinstance(event, str) or not 1 <= len(event) <= 128:
                        raise ValueError('invalid event')
                    if event in state['events']:
                        return self.reply(200 if state['events'][event] == body else 409)
                    job = state['jobs'].get(body['job_id'])
                    if not job or job.get('worker_id') != worker:
                        return self.reply(409)
                    # Legacy receipts are accepted only for legacy download claims.
                    expected_device = job.get('device_id', worker)
                    kind = job.get('type', 'download')
                    if device_id != expected_device or body.get('type', 'download') != kind:
                        return self.reply(409)
                    status = body['status']
                    allowed = ('probed',) if kind == 'probe' else ('started', 'uploaded', 'failed', 'cancelled', 'outcome_unknown')
                    if status not in allowed:
                        raise ValueError('invalid status')
                    if status == 'uploaded' and not job.get('sha256'):
                        return self.reply(409)
                    # Do not let delayed/conflicting receipts regress a terminal job.
                    if job.get('receipt_status') == status:
                        pass
                    elif job['status'] in TERMINAL and job['status'] != 'received':
                        return self.reply(409)
                    elif job.get('receipt_status') in ('uploaded', 'failed', 'cancelled', 'outcome_unknown'):
                        return self.reply(409)
                    job['receipt_status'] = status
                    job['receipt_at'] = stamp(utcnow())
                    if not job.get('sha256'):
                        job['status'] = status
                    if status == 'probed':
                        register(state, expected_device)['last_probed'] = job['receipt_at']
                    state['events'][event] = body
                    save(root / 'state.json', state)
                    return self.reply(200)
            except (ValueError, KeyError, TypeError):
                self.reply(400)
            except (OSError, TimeoutError):
                self.close_connection = True

        def do_PUT(self):
            self.connection.settimeout(120)
            with LOCK:
                state = load(root)
                job = next((j for j in state['jobs'].values() if self.path == j.get('upload_path')), None)
                if (not job or job.get('sha256') or not job.get('worker_id')
                        or job['status'] not in ('claimed', 'started')):
                    return self.reply(409)
                if datetime.now(timezone.utc) >= datetime.fromisoformat(job['lease_expires_at'].replace('Z', '+00:00')):
                    return self.reply(410)
                mime = 'audio/mpeg' if job['filetype'] == 'mp3' else 'audio/wav'
                if self.headers.get('Content-Type') != mime:
                    return self.reply(415)
                try:
                    data = self.body(LIMIT)
                    if not valid_audio(data, job['filetype']):
                        return self.reply(415)
                    destination = root / (job['job_id'] + '.' + job['filetype'])
                    partial = destination.with_suffix(destination.suffix + '.part')
                    try:
                        with partial.open('xb') as stream:
                            stream.write(data)
                            stream.flush()
                            os.fsync(stream.fileno())
                        # Link atomically without replacing an existing received file.
                        os.link(partial, destination)
                        partial.unlink()
                    finally:
                        partial.unlink(missing_ok=True)
                    job.update(status='received', file=str(destination.resolve()), bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
                    save(root / 'state.json', state)
                    self.reply(200)
                except (ValueError, OSError, TimeoutError):
                    self.close_connection = True
    return Handler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--state-dir', type=Path, default=Path.home() / '.suno' / 'remote-downloader')
    sub = parser.add_subparsers(dest='command', required=True)
    serve = sub.add_parser('serve')
    serve.add_argument('--port', type=int, default=8787)
    serve.add_argument('--token-file', type=Path, help='existing bearer token file (default: state-dir/worker-token)')
    enqueue = sub.add_parser('enqueue')
    enqueue.add_argument('content_id', type=uuid.UUID)
    enqueue.add_argument('--format', choices=['mp3', 'wav'], default='mp3')
    enqueue.add_argument('--port', type=int, default=8787)
    enqueue.add_argument('--device', type=identity)
    probe = sub.add_parser('probe', help='queue an idle-only connectivity probe')
    probe.add_argument('--device', required=True, type=identity)
    device = sub.add_parser('register', help='register a device before its first poll')
    device.add_argument('device_id', type=identity)
    move = sub.add_parser('reassign', help='change routing of a queued job only')
    move.add_argument('job_id')
    move.add_argument('--device', required=True, type=identity)
    sub.add_parser('distribute', help='balance queued downloads among recent idle pollers')
    sub.add_parser('status')
    sub.add_parser('config')
    args = parser.parse_args()
    root = args.state_dir.resolve()
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    process_lock = None
    if args.command not in ('status', 'config'):
        process_lock = (root / 'server.lock').open('a')
        try:
            fcntl.flock(process_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            parser.error('state is locked; stop the service before queue/device mutations (do not stop active work)')
    if args.command == 'serve':
        token_path = args.token_file or root / 'worker-token'
        if not token_path.exists():
            with open(token_path, 'x', opener=lambda p, f: os.open(p, f, 0o600)) as stream:
                stream.write(secrets.token_urlsafe(32))
        if not token_path.read_text().strip():
            parser.error('token file must not be empty')
        server = ThreadingHTTPServer(('127.0.0.1', args.port), handler(root, token_path.read_text().strip(), args.port))
        server.daemon_threads = True
        print(json.dumps({'listening': f'http://127.0.0.1:{args.port}', 'token_file': str(token_path)}), flush=True)
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass
        finally:
            server.server_close()
    elif args.command in ('enqueue', 'probe', 'register', 'reassign', 'distribute'):
        # All CLI mutations require the stopped-service process lock.
        state = load(root)
        try:
            if args.command == 'enqueue':
                job = new_job(state, args.port, args.content_id, args.format, args.device)
                result = {'job_id': job['job_id'], 'status': 'queued', 'device_id': args.device}
            elif args.command == 'probe':
                job = new_job(state, 0, device_id=args.device, kind='probe')
                result = {'job_id': job['job_id'], 'status': 'queued', 'device_id': args.device}
            elif args.command == 'register':
                result = register(state, args.device_id)
            elif args.command == 'reassign':
                job = reassign(state, args.job_id, args.device)
                result = {'job_id': job['job_id'], 'device_id': args.device, 'status': 'queued'}
            else:
                result = {'assignments': distribute(state)}
        except ValueError as error:
            parser.error(str(error))
        save(root / 'state.json', state)
        print(json.dumps(result))
    elif args.command == 'config':
        print(json.dumps({'claim_url': 'http://127.0.0.1:8787/jobs/claim', 'upload_origins': ['http://127.0.0.1:8787'], 'poll_seconds': 30, 'token_file': str(root / 'worker-token')}))
    else:
        state = load(root)
        print(json.dumps(status_view(state), indent=2))


if __name__ == '__main__':
    main()
