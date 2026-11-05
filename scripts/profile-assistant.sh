#!/usr/bin/env bash
# Read-only Linux daemon profiling. Python's standard library samples /proc;
# optional perf records CPU stacks. The pasteable report omits model paths and
# full argv; raw perf captures remain in a private temporary directory.
set -euo pipefail
command -v python3 >/dev/null || { echo 'python3 is required.' >&2; exit 1; }
exec python3 - "$@" <<'PY'
import argparse
import collections
import datetime
import os
import pathlib
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time


def clean(value):
    return ''.join(c if c.isprintable() else '?' for c in str(value))[:160]


def read(path):
    try:
        return pathlib.Path(path).read_text()
    except (OSError, UnicodeError):
        return None


def fields(path):
    return dict(line.split(':', 1) for line in (read(path) or '').splitlines() if ':' in line)


def number(values, key):
    try:
        return int(values[key].split()[0])
    except (KeyError, ValueError, IndexError):
        return None


def stat(path):
    text = read(path)
    if text is None:
        return None
    # comm can contain spaces and parentheses; numeric fields follow its last ')'.
    end = text.rfind(')')
    tail = text[end + 2:].split()
    try:
        return dict(name=clean(text[text.find('(') + 1:end]), state=tail[0],
                    minor=int(tail[7]), major=int(tail[9]), user=int(tail[11]),
                    system=int(tail[12]), threads=int(tail[17]), start=int(tail[19]),
                    rss=int(tail[21]) * os.sysconf('SC_PAGE_SIZE'))
    except (ValueError, IndexError):
        return None


def snapshot(pid):
    base = pathlib.Path('/proc') / str(pid)
    process = stat(base / 'stat')
    if process is None:
        return None
    tasks = {}
    try:
        paths = list((base / 'task').iterdir())
    except OSError:
        paths = []
    for path in paths:
        item = stat(path / 'stat')
        if item is None:
            continue
        status = fields(path / 'status')
        item['voluntary'] = number(status, 'voluntary_ctxt_switches')
        item['involuntary'] = number(status, 'nonvoluntary_ctxt_switches')
        sched = (read(path / 'schedstat') or '').split()
        item['runqueue_ns'] = int(sched[1]) if len(sched) >= 3 and sched[1].isdigit() else None
        item['wchan'] = clean((read(path / 'wchan') or '').strip())
        tasks[(int(path.name), item['start'])] = item
    process['tasks'] = tasks
    process['io'] = fields(base / 'io')
    return process


def system_snapshot():
    text = read('/proc/stat') or ''
    cpu = next((line.split()[1:9] for line in text.splitlines() if line.startswith('cpu ')), [])
    pressure = {}
    for kind in ('cpu', 'memory', 'io'):
        for line in (read('/proc/pressure/' + kind) or '').splitlines():
            parts = line.split()
            total = next((p[6:] for p in parts[1:] if p.startswith('total=')), None)
            if total is not None and total.isdigit():
                pressure[kind + '/' + parts[0]] = int(total)
    return dict(cpu=[int(v) for v in cpu], pressure=pressure)


def find_pid():
    # sudo should still discover the invoking user's daemon, not a root daemon.
    target_uid = os.getuid()
    if os.geteuid() == 0 and os.environ.get('SUDO_UID'):
        try:
            target_uid = int(os.environ['SUDO_UID'])
        except ValueError as error:
            raise RuntimeError('Invalid SUDO_UID; select the daemon with --pid PID.') from error
    candidates = []
    for path in pathlib.Path('/proc').iterdir():
        if not path.name.isdigit():
            continue
        try:
            if path.stat().st_uid != target_uid:
                continue
            executable = os.readlink(path / 'exe').removesuffix(' (deleted)')
            if pathlib.Path(executable).name != 'assistant':
                continue
            item = stat(path / 'stat')
            if item is not None:
                candidates.append((item['start'], int(path.name)))
        except OSError:
            continue
    if not candidates:
        raise RuntimeError('No assistant daemon found for the invoking user. Start it first, or pass --pid PID.')
    candidates.sort()
    if len(candidates) > 1:
        print('Found multiple daemons; selecting the newest. Use --pid to select another.', flush=True)
    return candidates[-1][1]


def delta(after, before):
    if after is None or before is None or after < before:
        return None
    return after - before


def shown(value, digits=1):
    return 'unavailable' if value is None else f'{value:.{digits}f}'


def stop_child(child):
    # Each optional profiler has its own process group; never signal the daemon.
    if child.poll() is None:
        try:
            os.killpg(child.pid, signal.SIGINT)
            child.wait(timeout=3)
        except (ProcessLookupError, subprocess.TimeoutExpired):
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()


def command(args, timeout=5):
    try:
        child = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 text=True, start_new_session=True)
        try:
            out, err = child.communicate(timeout=timeout)
            return child.returncode, out, err
        except subprocess.TimeoutExpired:
            stop_child(child)
            child.communicate()
            return 124, '', 'timed out'
    except OSError as error:
        return 127, '', str(error)


def main():
    parser = argparse.ArgumentParser(prog='profile-assistant.sh', description='Automatically profile assistant while you use its browser UI.')
    parser.add_argument('--pid', type=int, help='override automatic PID detection')
    parser.add_argument('--seconds', type=int, default=30, help='capture duration (default: 30)')
    parser.add_argument('--wait-seconds', type=int, default=120, help='time allowed to start a prompt (default: 120)')
    parser.add_argument('--immediate', action='store_true', help='start recording immediately')
    parser.add_argument('--no-perf', action='store_true', help='skip optional CPU stack sampling')
    parser.add_argument('--no-gpu', action='store_true', help='skip optional nvidia-smi device sampling')
    args = parser.parse_args()
    if not sys.platform.startswith('linux'):
        parser.error('this script requires Linux /proc')
    if not 1 <= args.seconds <= 300 or not 1 <= args.wait_seconds <= 600:
        parser.error('--seconds must be 1..300 and --wait-seconds 1..600')
    if args.pid is not None and args.pid <= 0:
        parser.error('--pid must be positive')
    pid = args.pid if args.pid is not None else find_pid()
    initial = snapshot(pid)
    if initial is None:
        raise RuntimeError(f'Cannot read PID {pid}.')
    identity = initial['start']
    hz = os.sysconf('SC_CLK_TCK')
    os.umask(0o077)
    directory = pathlib.Path(tempfile.mkdtemp(prefix='assistant-profile-'))
    print(f'Assistant PID: {pid}. Report directory: {directory}', flush=True)
    print('Switch to the browser and ask for a long answer. Do not restart the daemon during capture.', flush=True)
    interrupted = threading.Event()
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, lambda *_: interrupted.set())
    if not args.immediate:
        print(f'Waiting up to {args.wait_seconds}s for sustained CPU activity (>=15% of one core)...', flush=True)
        previous, previous_at = initial, time.monotonic()
        deadline = previous_at + args.wait_seconds
        busy = 0
        while time.monotonic() < deadline and not interrupted.is_set():
            interrupted.wait(0.5)
            current, now = snapshot(pid), time.monotonic()
            if current is None or current['start'] != identity:
                raise RuntimeError('Daemon exited or restarted before capture; rerun the script.')
            used = current['user'] + current['system'] - previous['user'] - previous['system']
            cpu = used / hz / (now - previous_at) * 100
            busy = busy + 1 if cpu >= 15 else 0
            previous, previous_at = current, now
            if busy >= 2:
                break
        if busy < 2:
            raise RuntimeError('No sustained activity detected. Retry with --immediate to force capture.')

    start = snapshot(pid)
    if start is None or start['start'] != identity:
        raise RuntimeError('Daemon exited before capture.')
    started = time.monotonic()
    born_after = int(time.clock_gettime(time.CLOCK_BOOTTIME) * hz)
    deadline = started + args.seconds
    system_start = system_snapshot()
    profile_stop = threading.Event()
    perf_result = {'status': 'disabled' if args.no_perf else 'perf is not installed'}
    gpu_results = []

    def profile():
        failures = []
        for event in ('cpu-clock:uk', 'cpu-clock:u'):
            remaining = deadline - time.monotonic()
            if remaining < 1 or profile_stop.is_set():
                break
            log_path = directory / ('perf-' + event.replace(':', '-') + '.log')
            with log_path.open('w') as log:
                try:
                    child = subprocess.Popen(['perf', 'record', '-q', '-e', event, '-F', '99',
                                              '--call-graph', 'dwarf', '-p', str(pid), '-o',
                                              str(directory / 'perf.data'), '--', 'sleep', f'{remaining:.3f}'],
                                             stdout=subprocess.DEVNULL, stderr=log, start_new_session=True)
                except OSError as error:
                    failures.append(clean(error))
                    continue
                while child.poll() is None and not profile_stop.wait(0.1):
                    if time.monotonic() > deadline + 2:
                        break
                stop_child(child)
            if child.returncode == 0 or (directory / 'perf.data').exists() and (directory / 'perf.data').stat().st_size > 1024:
                rc, output, error = command(['perf', 'report', '-i', str(directory / 'perf.data'),
                                             '--stdio', '--no-children', '--call-graph', 'none',
                                             '--sort', 'comm,dso,symbol', '--percent-limit', '1'], timeout=15)
                if rc == 0 and output.strip() and '# Samples:' in output:
                    perf_result.update(status='captured (requested ' + event + '; actual scope appears below)', report=output)
                    # Opaque driver work can be spread across hundreds of symbols,
                    # each below the symbol report's threshold. Show library totals
                    # before symbols so this work stays visible in the pasted report.
                    group_rc, group_output, group_error = command([
                        'perf', 'report', '-i', str(directory / 'perf.data'),
                        '--stdio', '--no-children', '--call-graph', 'none',
                        '--sort', 'comm,dso', '--percent-limit', '0.5'], timeout=15)
                    if group_rc == 0 and '# Samples:' in group_output:
                        perf_result['libraries'] = group_output
                    else:
                        perf_result['library_error'] = clean(group_error or 'no grouped CPU samples')
                    break
                failures.append(clean(error or 'no CPU samples'))
            else:
                failures.append(clean((read(log_path) or 'recording failed').strip()))
        if 'report' not in perf_result:
            perf_result['status'] = 'unavailable: ' + '; '.join(failures or ['capture stopped'])

    def gpu_sample():
        while time.monotonic() < deadline and not profile_stop.is_set():
            rc, out, err = command(['nvidia-smi', '--query-gpu=index,utilization.gpu,memory.used,power.draw',
                                    '--format=csv,noheader,nounits'], timeout=2)
            gpu_results.append((time.monotonic() - started, rc, out.strip() if rc == 0 else clean(err)))
            if rc != 0 or profile_stop.wait(3):
                break

    workers = []
    if not args.no_perf and shutil.which('perf'):
        workers.append(threading.Thread(target=profile))
    if not args.no_gpu and shutil.which('nvidia-smi'):
        workers.append(threading.Thread(target=gpu_sample))
    for worker in workers:
        worker.start()
    print(f'RECORDING for {args.seconds}s. Keep the answer running; Ctrl-C ends capture early.', flush=True)
    totals = collections.defaultdict(lambda: collections.Counter())
    names, waits = {}, collections.Counter()
    previous, previous_at = start, started
    rows, observations = [], 0
    process_stopped = False
    while time.monotonic() < deadline and not interrupted.is_set():
        interrupted.wait(min(1, max(0, deadline - time.monotonic())))
        current, now = snapshot(pid), time.monotonic()
        if current is None or current['start'] != identity:
            process_stopped = True
            break
        span = now - previous_at
        user = (current['user'] - previous['user']) / hz
        system = (current['system'] - previous['system']) / hz
        voluntary = involuntary = queue_ns = 0
        for key, item in current['tasks'].items():
            names[key] = item['name']
            old = previous['tasks'].get(key)
            if old is None and item['start'] >= born_after:
                old = {field: 0 for field in ('user', 'system', 'voluntary', 'involuntary', 'runqueue_ns')}
            if old is not None:
                for field in ('user', 'system', 'voluntary', 'involuntary', 'runqueue_ns'):
                    amount = delta(item[field], old[field])
                    if amount is not None:
                        totals[key][field] += amount
                        totals[key][field + '_available'] = 1
                        if field == 'voluntary': voluntary += amount
                        if field == 'involuntary': involuntary += amount
                        if field == 'runqueue_ns': queue_ns += amount
            if item['state'] != 'R' and item['wchan'] not in ('', '0'):
                waits[(item['name'], item['wchan'])] += 1
        rows.append((now - started, user / span * 100, system / span * 100,
                     voluntary / span, involuntary / span, queue_ns / 1e6 / span,
                     current['rss'] / 1048576, current['threads'],
                     current['major'] - previous['major']))
        observations += 1
        previous, previous_at = current, now
    ended = previous_at
    system_end = system_snapshot()
    profile_stop.set()
    for worker in workers:
        worker.join()
    elapsed = ended - started
    if elapsed <= 0 or not rows:
        raise RuntimeError('Capture ended before any sample was collected.')

    lines = ['===== ASSISTANT CPU PROFILE =====',
             'UTC: ' + datetime.datetime.now(datetime.timezone.utc).isoformat(timespec='seconds'),
             f'PID: {pid}; elapsed: {elapsed:.2f}s; samples: {observations}; logical CPUs: {os.cpu_count()}',
             '100% process CPU = one logical CPU; browser CPU is excluded.',
             'Scheduling counters come from /proc, independent of perf event filtering.']
    if process_stopped:
        lines.append('Daemon exited/restarted: this is a partial capture of the original PID.')
    executable = ''
    try:
        executable = os.readlink(f'/proc/{pid}/exe')
    except OSError:
        pass
    build = 'debug' if '/target/debug/' in executable else 'release' if '/target/release/' in executable else 'unknown'
    lines.append('Executable location suggests build: ' + build)
    args_raw = pathlib.Path(f'/proc/{pid}/cmdline')
    try:
        argv = args_raw.read_bytes().split(b'\0')
        allowed = {'--ctx-size', '--batch-size', '--ubatch-size', '--threads', '--gpu-layers',
                   '--cache-type-k', '--cache-type-v', '--swa-cache', '--flash-attn', '--attn-rotation'}
        selected = []
        for i, raw in enumerate(argv):
            arg = raw.decode(errors='replace')
            key, sep, value = arg.partition('=')
            if key in allowed:
                value = value if sep else argv[i + 1].decode(errors='replace') if i + 1 < len(argv) else '?'
                selected.append(key + '=' + (value if re.fullmatch(r'[A-Za-z0-9_.-]+', value) else '<redacted>'))
            elif arg == '--cpu':
                selected.append('--cpu')
        lines.append('Explicit settings (omitted flags use defaults): ' + (' '.join(selected) or 'none'))
    except OSError:
        lines.append('Explicit settings: unavailable')
    user = (previous['user'] - start['user']) / hz
    system = (previous['system'] - start['system']) / hz
    lines += ['', f'PROCESS: user {user:.3f}s ({user / elapsed * 100:.1f}%); '
              f'system {system:.3f}s ({system / elapsed * 100:.1f}%); total {(user + system) / elapsed * 100:.1f}%',
              f'RSS: start {start["rss"] / 1048576:.1f} MiB; end {previous["rss"] / 1048576:.1f} MiB; '
              f'max sampled {max(start["rss"] / 1048576, *(r[6] for r in rows)):.1f} MiB',
              f'Faults: minor {previous["minor"] - start["minor"]}; major {previous["major"] - start["major"]}']
    for key in ('read_bytes', 'write_bytes', 'syscr', 'syscw'):
        value = delta(number(previous['io'], key), number(start['io'], key))
        lines.append(f'I/O {key} delta: {shown(value, 0)}')
    cpu_before, cpu_after = system_start['cpu'], system_end['cpu']
    if len(cpu_before) == len(cpu_after) == 8:
        differences = [b - a for a, b in zip(cpu_before, cpu_after)]
        total = sum(differences)
        if total > 0:
            lines.append(f'MACHINE CPU: busy {(total - differences[3] - differences[4]) / total * 100:.1f}%; '
                         f'iowait {differences[4] / total * 100:.1f}% (whole machine, includes other apps)')
    for key, after in system_end['pressure'].items():
        amount = delta(after, system_start['pressure'].get(key))
        lines.append(f'Machine PSI {key}: {shown(None if amount is None else amount / elapsed / 10000)}% stalled wall time')
    lines += ['', 'INTERVALS (one-second samples; CPU totals may exceed 100%):',
              ' end_s   user%    sys%    csw/s   invol/s  runqueue_ms/s   RSS_MiB threads major_faults']
    for row in rows:
        lines.append(f'{row[0]:6.1f} {row[1]:7.1f} {row[2]:7.1f} {row[3]:8.1f} {row[4]:9.1f} '
                     f'{row[5]:14.2f} {row[6]:9.1f} {row[7]:7d} {row[8]:12d}')
    lines += ['', 'TOP THREADS (averaged over the full capture):',
              '    TID   user%    sys%   csw/s  invol/s  runqueue_ms   name']
    for key, item in sorted(totals.items(), key=lambda pair: pair[1]['user'] + pair[1]['system'], reverse=True)[:20]:
        lines.append(f'{key[0]:7d} {item["user"] / hz / elapsed * 100:7.1f} '
                     f'{item["system"] / hz / elapsed * 100:7.1f} '
                     f'{shown(item["voluntary"] / elapsed if item["voluntary_available"] else None):>7} '
                     f'{shown(item["involuntary"] / elapsed if item["involuntary_available"] else None):>8} '
                     f'{shown(item["runqueue_ns"] / 1e6 if item["runqueue_ns_available"] else None, 2):>12} '
                     f'{names.get(key, "?")}')
    lines += ['Runqueue time = time runnable but waiting for a CPU, summed per thread.',
              'Zero schedstat values can mean scheduler accounting is disabled.',
              'Context switches are observed thread deltas; short-lived/exited threads can be missed.',
              '', 'SAMPLED BLOCKED THREAD LOCATIONS (counts, not time):']
    for (name, location), count in waits.most_common(12):
        lines.append(f'{count:5d} {name}: {location}')
    lines += ['', 'OPTIONAL NVIDIA DEVICE SAMPLES (all workloads, not daemon-only):']
    if not gpu_results:
        lines.append('not collected (disabled or nvidia-smi unavailable)')
    for at, rc, output in gpu_results:
        if rc == 0:
            lines.append(f'{at:.1f}s: device index, GPU busy %, device memory MiB, power W: {clean(output)}')
        else:
            lines.append('unavailable: ' + output)
    lines += ['', 'CPU HOTSPOTS: ' + perf_result['status'],
              'The perf event name shows its scope: :u excludes kernel stacks; /proc system CPU remains available.']
    if 'libraries' in perf_result:
        libraries = perf_result['libraries']
        (directory / 'perf-libraries.txt').write_text(libraries)
        lines.append('LIBRARY TOTALS (percent of sampled CPU time, not percent of one core):')
        lines.extend(libraries.splitlines()[:100])
        if len(libraries.splitlines()) > 100:
            lines.append('(truncated; full library totals saved alongside report.txt)')
    elif 'library_error' in perf_result:
        lines.append('Library totals unavailable: ' + perf_result['library_error'])
    if 'report' in perf_result:
        report = perf_result['report']
        (directory / 'perf-report.txt').write_text(report)
        lines.append('SYMBOL TOTALS:')
        lines.extend(report.splitlines()[:100])
        if len(report.splitlines()) > 100:
            lines.append('(truncated; full report saved alongside report.txt)')
    lines += ['', 'Artifacts: ' + str(directory), '===== END ASSISTANT CPU PROFILE =====']
    report = '\n'.join(lines) + '\n'
    (directory / 'report.txt').write_text(report)
    print('\n' + report, flush=True)


try:
    main()
except (OSError, RuntimeError) as error:
    print('Error: ' + clean(error), file=sys.stderr)
    sys.exit(1)
PY
