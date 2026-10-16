"""Prepare, run and archive repeatable Agent Skill validation attempts."""

from dataclasses import asdict
from datetime import datetime, timezone
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import tempfile
from urllib.parse import urlsplit

from agent_skill_audit import audit_trace, preview_reports
from agent_skill_events import parse_events, read_events


AGENTS = ('claude', 'codex', 'pi', 'opencode')
SCENARIOS = ('allowed', 'denied', 'pipeline-denied')
OPENING = ('This project uses apexe-governed external CLI execution, with the project\n'
           'working directory and reviewed default bindings and ACL under ~/.apexe.')


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat()


def digest(content: bytes) -> str:
    return hashlib.sha256(content).hexdigest()


def write_json(path: Path, value: dict) -> None:
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, delete=False) as stream:
            temporary = Path(stream.name)
            json.dump(value, stream, indent=2, ensure_ascii=False)
            stream.write('\n')
        temporary.chmod(0o600)
        temporary.replace(path)
    finally:
        if temporary:
            temporary.unlink(missing_ok=True)


def validate_local_endpoint(endpoint: str) -> None:
    url = urlsplit(endpoint)
    local = url.hostname == 'localhost'
    try:
        local = local or ipaddress.ip_address(url.hostname or '').is_loopback
    except ValueError:
        pass
    if url.scheme not in ('http', 'https') or not local or url.username or url.password or url.query or url.fragment:
        raise ValueError('OpenCode endpoint must be a loopback URL without credentials')


def documented_materials(repository: Path) -> tuple[str, str, list[str]]:
    guide = (repository/'docs/agent-skill.md').read_text()
    verification = (repository/'docs/agent-skill-verification.md').read_text()
    template = re.search(r'```markdown\n(.*?)```', guide, re.S)
    prompt = re.search(r'## 3\. Run the three scenarios.*?\n> ([^\n]+)', verification, re.S)
    section = verification.split('## 2. Prepare disposable fixtures and policies', 1)[1].split('## 3.', 1)[0]
    policies = re.findall(r'```yaml\n(.*?)```', section, re.S)
    if not template or not template[1].startswith(OPENING) or not prompt or len(policies) != 3:
        raise ValueError('Published template, prompt or three ACL fixtures changed; update the validator')
    return template[1], prompt[1], policies


def skill_locations(scope: str, agent: str, cwd: Path, home: Path) -> list[Path]:
    if scope == 'user':
        paths = [home/'.claude/skills/apexe/SKILL.md'] if agent == 'claude' else [home/'.agents/skills/apexe/SKILL.md']
        return paths+[home/'.claude/skills/apexe/SKILL.md'] if agent == 'opencode' else paths
    directory = '.claude' if agent == 'claude' else '.opencode' if agent == 'opencode' else '.agents'
    return [cwd/directory/'skills/apexe/SKILL.md']


def create_case(root: Path, options: dict, agent: str, scenario: str, policy: str,
                template: str, source: bytes) -> dict:
    cwd = root/'projects'/agent/scenario
    cwd.mkdir(parents=True)
    acl = cwd/'test-acl.yaml'
    bindings = options['bindings']
    override = options.get('bindings_override')
    config = f'This project uses apexe-governed external CLI execution, with working directory\n{cwd}, reviewed '
    config += f'bindings at {bindings}' if override else f'default bindings under {bindings}'
    config += f',\nand the explicitly selected ACL at {acl}.'
    if override:
        config += f' The selected bindings override is --bindings-dir `{override}`.'
    instructions = template.replace(OPENING, config, 1)
    filename = 'CLAUDE.md' if agent == 'claude' else 'AGENTS.md'
    files = {filename: instructions, 'test-acl.yaml': policy,
             'visible-marker.txt': 'visible fixture\n', '.hidden-marker.txt': 'hidden fixture\n',
             'input.txt': 'first-line\nsecond-line\nthird-line\n'}
    for name, text in files.items():
        (cwd/name).write_text(text)
    locations = skill_locations(options['scope'], agent, cwd, Path(options['home']))
    if options['scope'] == 'project':
        locations[0].parent.mkdir(parents=True)
        locations[0].write_bytes(source)
    return {'agent': agent, 'scenario': scenario, 'cwd': str(cwd), 'acl': str(acl),
            'bindings': bindings, 'bindings_override': override, 'home': options['home'],
            'project_instructions': instructions, 'skill_paths': list(map(str, locations)),
            'fixture_hashes': {name: digest(text.encode()) for name, text in files.items()}}


def prepare_run(repository: Path, report_dir: Path, options: dict) -> Path:
    validate_local_endpoint(options['opencode_endpoint'])
    if not options['opencode_model'].startswith('ollama/'):
        raise ValueError('OpenCode model must use the local ollama provider')
    if any('\n' in options[name] or '\r' in options[name] for name in ('home', 'bindings')):
        raise ValueError('Configuration paths must not contain newlines')
    template, prompt, policies = documented_materials(repository)
    report_dir = report_dir.expanduser().resolve()
    report_dir.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix=datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ-'), dir=report_dir))
    root.chmod(0o700)
    source = (repository/'skills/apexe/SKILL.md').read_bytes()
    (root/'source-SKILL.md').write_bytes(source)
    for name in ('agent-skill.md', 'agent-skill-verification.md'):
        (root/('source-'+name)).write_bytes((repository/'docs'/name).read_bytes())
    cases = [create_case(root, options, agent, scenario, policy, template, source)
             for agent in options['agents'] for scenario, policy in zip(SCENARIOS, policies)]
    manifest = {'schema_version': 1, 'created_utc': utc_now(), 'root': str(root),
                'report_dir': str(report_dir), 'skill_sha256': digest(source),
                'guide_sha256': digest((repository/'docs/agent-skill.md').read_bytes()),
                'prompt': prompt, 'options': options, 'cases': cases}
    manifest['validator_hashes'] = {path.name: digest(path.read_bytes()) for path in
        [repository/'scripts/validate_agent_skill.py', *sorted((repository/'scripts').glob('agent_skill_*.py'))]}
    write_json(root/'manifest.json', manifest)
    (root/'manifest.sha256').write_text(digest((root/'manifest.json').read_bytes())+'\n')
    review = ['# Agent Skill validation materials', f'Skill SHA-256: `{manifest["skill_sha256"]}`',
              'The run sends these fixture materials, Skill and ordinary task prompt through the selected hosts, which may also attach their normal system context and Skill metadata. Credentials are inherited by the clients, never copied into this review. Raw native logs stay in this private run directory.',
              '## Host/model configuration', json.dumps(options, indent=2), '## Ordinary task', prompt,
              '## Skill', source.decode(), '## Projects and ACLs']
    for case in cases:
        review += [f'### {case["agent"]} / {case["scenario"]}', case['project_instructions'],
                   (Path(case['cwd'])/'test-acl.yaml').read_text()]
    (root/'review.md').write_text('\n\n'.join(review)+'\n')
    return root


def preserve_first_failure(report_dir: Path, result: dict) -> None:
    if result['status'] == 'passed':
        return
    names = ['first-failure.json']
    if result['status'] == 'routing_failure':
        names.append('first-routing-failure.json')
    for name in names:
        try:
            with (report_dir/name).open('x') as stream:
                json.dump(result, stream, indent=2, ensure_ascii=False)
                stream.write('\n')
            (report_dir/name).chmod(0o600)
        except FileExistsError:
            pass


def binding_hashes(directory: Path) -> dict:
    if not directory.is_dir():
        raise ValueError(f'Reviewed bindings directory is missing: {directory}')
    return {str(p.relative_to(directory)): digest(p.read_bytes())
            for p in sorted(directory.rglob('*.binding.yaml')) if p.is_file()}


def check_case(case: dict, manifest: dict) -> None:
    for filename, expected in case['fixture_hashes'].items():
        path = Path(case['cwd'])/filename
        if not path.is_file() or digest(path.read_bytes()) != expected:
            raise ValueError(f'Fixture changed: {filename}; prepare a fresh run')
    paths = [Path(p) for p in case['skill_paths']]
    present = [p for p in paths if p.is_file()]
    if not present or any(digest(p.read_bytes()) != manifest['skill_sha256'] for p in present):
        raise ValueError('Selected Skill installation is missing or differs from the frozen source')
    if manifest['options']['scope'] == 'user' and list(Path(case['cwd']).glob('**/SKILL.md')):
        raise ValueError('User-discovery fixture contains a project Skill copy')


def opencode_config(manifest: dict, case: dict) -> dict:
    options = manifest['options']
    config = {'enabled_providers': ['ollama'], 'model': options['opencode_model'],
              'small_model': options['opencode_model'], 'share': 'disabled',
              'provider': {'ollama': {'options': {'baseURL': options['opencode_endpoint']}}},
              'permission': {'read': {case['acl']: 'allow'}}}
    if options['scope'] == 'project':
        config['skills'] = {'paths': [str(Path(case['skill_paths'][0]).parents[1])]}
    return config


def launch_arguments(manifest: dict, case: dict) -> tuple[list[str], dict]:
    options, agent = manifest['options'], case['agent']
    prompt, cwd = manifest['prompt'], case['cwd']
    environment = dict(os.environ)
    if agent == 'claude':
        command = ['claude', '-p', prompt, '--output-format', 'stream-json', '--verbose',
                   '--no-session-persistence', '--allowedTools', 'Bash', 'Read', 'Skill',
                   '--settings', '{"disableAllHooks":true}', '--setting-sources', 'user,project',
                   '--strict-mcp-config', '--mcp-config', '{"mcpServers":{}}']
    elif agent == 'codex':
        command = ['codex', '-a', 'never', 'exec', '--json', '--skip-git-repo-check',
                   '--sandbox', 'workspace-write', '--add-dir', options['bindings'], '--cd', cwd, prompt]
    elif agent == 'pi':
        command = ['pi', '--mode', 'json', '--print', '--no-session', '--offline',
                   '--no-extensions', '--no-mcp', '--no-prompt-templates', '--approve', prompt]
    else:
        command = ['opencode', 'run', '--pure', '--format', 'json', '--model', options['opencode_model'], '--dir', cwd, prompt]
        environment['OPENCODE_CONFIG_CONTENT'] = json.dumps(opencode_config(manifest, case))
        for key in ('OPENCODE_DISABLE_EXTERNAL_SKILLS', 'OPENCODE_DISABLE_CLAUDE_CODE_SKILLS'):
            if options['scope'] == 'project':
                environment[key] = '1'
            else:
                environment.pop(key, None)
    requested = options.get(agent+'_model')
    if requested and agent != 'opencode':
        command += ['--model', requested]
    return command, environment


def stop_process(process: subprocess.Popen) -> None:
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        process.wait()
        return
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()


def capture_process(command: list[str], cwd: Path, environment: dict, output: Path,
                    errors: Path, timeout: float) -> dict:
    started = utc_now()
    with output.open('x') as stdout, errors.open('x') as stderr:
        process = subprocess.Popen(command, cwd=cwd, env=environment, stdout=stdout,
                                   stderr=stderr, text=True, start_new_session=True)
        try:
            exit_code = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            stop_process(process)
            exit_code = 'timeout'
        except BaseException:
            stop_process(process)
            raise
        stop_process(process)
    return {'started_utc': started, 'finished_utc': utc_now(), 'process_exit': exit_code}


def capture_supplement(agent: str, events: list[dict], case: dict, environment: dict, attempt: Path) -> dict:
    if agent == 'codex':
        identifiers = {e['thread_id'] for e in events if e.get('type') == 'thread.started'}
        if len(identifiers) != 1:
            raise ValueError('Expected one owned Codex thread ID')
        identifier = next(iter(identifiers))
        if not re.fullmatch(r'[A-Za-z0-9-]+', identifier):
            raise ValueError('Invalid owned Codex thread ID')
        home = Path(os.environ.get('CODEX_HOME', str(Path(case['home'])/'.codex')))
        paths = list((home/'sessions').rglob(f'*{identifier}*.jsonl'))
        if len(paths) != 1:
            raise ValueError('Owned Codex native rollout unavailable; keep the non-ephemeral session')
        shutil.copyfile(paths[0], attempt/'rollout.jsonl')
        return {'native': read_events(attempt/'rollout.jsonl')}
    if agent == 'opencode':
        identifiers = {e['sessionID'] for e in events if e.get('sessionID')}
        if len(identifiers) != 1 or not re.fullmatch(r'[A-Za-z0-9_-]+', next(iter(identifiers))):
            raise ValueError('Expected one owned OpenCode session ID')
        command = ['opencode', 'export', next(iter(identifiers)), '--pure', '--sanitize']
        result = subprocess.run(command, cwd=case['cwd'], env=environment, capture_output=True,
                                text=True, timeout=60, check=True)
        payload = json.loads(result.stdout[result.stdout.index('{'):])
        models = [{k: m['info'].get(k) for k in ('providerID', 'modelID')}
                  for m in payload['messages'] if m['info'].get('role') == 'assistant']
        write_json(attempt/'actual-model.json', {'assistant_models': models})
        return {'models': models}
    return {}


def check_native_prerequisites(case: dict, attempt: Path) -> None:
    command = ['apexe', 'exec', '--acl', case['acl']]
    if case.get('bindings_override'):
        command += ['--bindings-dir', case['bindings_override']]
    results = []
    for child in (['ls', '-la'], ['cat', 'input.txt'], ['head', '-n', '2']):
        result = subprocess.run(command+['--dry-run', '--', *child], cwd=case['cwd'],
                                text=True, capture_output=True, timeout=30, check=False)
        reports = preview_reports(result.stdout+'\n'+result.stderr)
        results.append({'child': child, 'exit_code': result.returncode, 'reports': reports})
        if result.returncode != 0 or not any(p.get('valid') is True and p.get('mode') == 'native'
                                           and p.get('acl_decision') == 'allowed' for p in reports):
            write_json(attempt/'prerequisites.json', {'results': results})
            raise ValueError('Reviewed ls/cat/head native prerequisites are not admitted; no Agent was launched')
    write_json(attempt/'prerequisites.json', {'results': results})


def run_case(root: Path, manifest: dict, case: dict, timeout: float) -> dict:
    attempt = root/'attempts'/case['agent']/case['scenario']
    attempt.mkdir(parents=True)
    result = {'agent': case['agent'], 'scenario': case['scenario'], 'created_utc': utc_now(),
              'skill_sha256': manifest['skill_sha256'], 'attempt': str(attempt), 'status': 'prerequisite_failure'}
    try:
        check_case(case, manifest)
        before = binding_hashes(Path(case['bindings']))
        version = subprocess.check_output([case['agent'], '--version'], text=True, timeout=30).strip()
        result['agent_version'] = version
        result['apexe_version'] = subprocess.check_output(['apexe', '--version'], text=True, timeout=30).strip()
        command, environment = launch_arguments(manifest, case)
        write_json(attempt/'launch.json', {'arguments': command, 'agent_version': version,
                                         'opencode_config': opencode_config(manifest, case) if case['agent']=='opencode' else None})
        result.update(capture_process(command, Path(case['cwd']), environment,
                      attempt/'events.jsonl', attempt/'stderr.log', timeout))
        native = read_events(attempt/'events.jsonl')
        supplement_error = None
        try:
            supplement = capture_supplement(case['agent'], native, case, environment, attempt)
        except (ValueError, OSError, subprocess.SubprocessError) as error:
            supplement, supplement_error = {}, str(error)
        trace = parse_events(case['agent'], native, **supplement)
        if supplement_error:
            trace.errors.append(supplement_error)
        write_json(attempt/'normalized.json', asdict(trace))
        case = dict(case, project_autoload=case['agent']=='claude' and
                    command[command.index('--setting-sources')+1] == 'user,project')
        result.update(audit_trace(trace, case, (root/'source-SKILL.md').read_text()))
        try:
            check_case(case, manifest)
            unchanged = binding_hashes(Path(case['bindings'])) == before
            if not unchanged:
                raise ValueError('Reviewed bindings changed during the run')
            result['checks']['fixtures_and_bindings_unchanged'] = True
        except (OSError, ValueError) as error:
            result['status'] = 'routing_failure'
            result['checks']['fixtures_and_bindings_unchanged'] = False
            result['error'] = str(error)
        if result['process_exit'] != 0:
            if result['status'] != 'routing_failure':
                result['status'] = 'prerequisite_failure'
            result['error'] = f'Agent process failed: {result["process_exit"]}'
        if case['agent'] == 'opencode' and result['actual_models'] and result['actual_models'] != [manifest['options']['opencode_model']]:
            if result['status'] != 'routing_failure':
                result['status'] = 'prerequisite_failure'
            result['error'] = 'Observed OpenCode model differs from the selected local model'
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        if result['status'] != 'routing_failure':
            result['status'] = 'prerequisite_failure'
        result['error'] = str(error)
    write_json(attempt/'receipt.json', result)
    preserve_first_failure(Path(manifest['report_dir']), result)
    return result


def run_prepared(root: Path, agents: list[str], timeout: float) -> dict:
    if digest((root/'manifest.json').read_bytes()) != (root/'manifest.sha256').read_text().strip():
        raise ValueError('Frozen manifest changed; prepare a new run')
    manifest = json.loads((root/'manifest.json').read_text())
    if manifest['schema_version'] != 1:
        raise ValueError('Unsupported prepared manifest schema')
    for filename, expected in manifest['validator_hashes'].items():
        if digest((Path(__file__).parent/filename).read_bytes()) != expected:
            raise ValueError('Validator changed since preparation; prepare a fresh run')
    if digest((root/'source-SKILL.md').read_bytes()) != manifest['skill_sha256']:
        raise ValueError('Frozen Skill source changed; prepare a new run')
    results = []
    for agent in agents:
        if len([c for c in manifest['cases'] if c['agent']==agent]) != 3:
            raise ValueError(f'Agent not prepared: {agent}')
        if (root/'attempts'/agent).exists():
            raise ValueError(f'Attempt already exists for {agent}; prepare a fresh run to preserve it')
    for agent in agents:
        cases = [case for case in manifest['cases'] if case['agent'] == agent]
        if len(cases) != 3:
            raise ValueError(f'Agent not prepared: {agent}')
        if (root/'attempts'/agent).exists():
            raise ValueError(f'Attempt already exists for {agent}; prepare a fresh run to preserve it')
        prerequisites = root/'prerequisites'/agent
        prerequisites.mkdir(parents=True)
        version = 'unavailable'
        try:
            version = subprocess.check_output([agent, '--version'], text=True, timeout=30).strip()
            check_case(cases[0], manifest)
            binding_hashes(Path(cases[0]['bindings']))
            check_native_prerequisites(cases[0], prerequisites)
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            for case in cases:
                attempt = root/'attempts'/agent/case['scenario']
                attempt.mkdir(parents=True)
                result = {'agent': agent, 'scenario': case['scenario'], 'status': 'prerequisite_failure',
                          'error': str(error), 'agent_version': version, 'actual_models': [],
                          'skill_sha256': manifest['skill_sha256'], 'attempt': str(attempt)}
                write_json(attempt/'receipt.json', result)
                preserve_first_failure(Path(manifest['report_dir']), result)
                results.append(result)
            continue
        for case in cases:
            print(json.dumps({'agent': agent, 'scenario': case['scenario'], 'status': 'running'}), flush=True)
            result = run_case(root, manifest, case, timeout)
            results.append(result)
            print(json.dumps({k: result.get(k) for k in ('agent', 'scenario', 'status', 'actual_models')}), flush=True)
    return aggregate(root)


def aggregate(root: Path) -> dict:
    manifest = json.loads((root/'manifest.json').read_text())
    results = [json.loads(path.read_text()) for path in sorted((root/'attempts').glob('*/*/receipt.json'))]
    counts = {status: sum(r['status']==status for r in results)
              for status in ('passed', 'routing_failure', 'prerequisite_failure', 'unverified')}
    receipt = {'schema_version': 1, 'skill_sha256': manifest['skill_sha256'],
               'scope': manifest['options']['scope'], 'completed': len(results), 'expected': len(manifest['cases']),
               'counts': counts, 'results': results}
    write_json(root/'receipt.json', receipt)
    rows = ['# Agent Skill verification', '', f'Skill SHA-256: `{manifest["skill_sha256"]}`', '',
            '| Agent | Version | Scenario | Actual models | Result |', '|---|---|---|---|---|']
    for result in results:
        rows.append('| '+' | '.join(str(result.get(k, 'unverified')).replace('|', '\\|') for k in
                     ('agent', 'agent_version', 'scenario', 'actual_models', 'status'))+' |')
    rows += ['', f'Completed: {len(results)}/{len(manifest["cases"])}. Counts: `{json.dumps(counts)}`.', '',
             'Native logs, normalized traces and immutable per-scenario receipts are under `attempts/`.',
             'Prepare a fresh run for any retry. The report directory retains `first-failure.json` across revisions.', '']
    (root/'report.md').write_text('\n'.join(rows))
    return receipt
