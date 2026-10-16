"""Normalize native Agent events without executing recorded shell or JavaScript."""

from dataclasses import dataclass, field
import ast
import json
from pathlib import Path
import re
import shlex


@dataclass
class ToolCall:
    identifier: str
    response: str
    tool: str
    request_index: int
    result_index: int = -1
    command: str = ''
    output: str = ''
    exit_code: int | None = None
    error: bool = False
    arguments: dict = field(default_factory=dict)
    weight: int = 1


@dataclass
class Evidence:
    index: int
    path: str
    text: str


@dataclass
class Decision:
    index: int
    response: str
    text: str


@dataclass
class Trace:
    calls: list[ToolCall] = field(default_factory=list)
    models: list[str] = field(default_factory=list)
    evidence: list[Evidence] = field(default_factory=list)
    decisions: list[Decision] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)
    cwd: str = ''


def read_events(path: Path) -> list[dict]:
    events = []
    for number, line in enumerate(path.read_text().splitlines(), 1):
        if not line.strip():
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError as error:
            raise ValueError(f'Invalid native JSON at {path.name}:{number}') from error
        if not isinstance(value, dict):
            raise ValueError(f'Expected native event object at {path.name}:{number}')
        events.append(value)
    return events


def content_text(value) -> str:
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        return '\n'.join(p.get('text', '') for p in value if isinstance(p, dict)
                         and p.get('type') in ('text', 'input_text'))
    return ''


def reported_status(output: str) -> int | None:
    pattern = r'(?im)^(?:Exit code[: ]+|Command exited with code |(?:pipeline_status|pipeline_exit|PIPELINE_EXIT|exit)=)(-?\d+)'
    match = re.search(pattern, output)
    return int(match[1]) if match else None


def unwrap_host_shell(command: str) -> str:
    tokens = shlex.split(command)
    if tokens and Path(tokens[0]).name in ('bash', 'sh', 'zsh'):
        for option in ('-lc', '-c'):
            if option in tokens and tokens.index(option)+1 < len(tokens):
                return tokens[tokens.index(option)+1]
    return command


def attach_evidence(trace: Trace, call: ToolCall) -> None:
    path = next((str(call.arguments[k]) for k in ('filePath', 'filepath', 'file_path', 'path')
                 if k in call.arguments), call.command)
    trace.evidence.append(Evidence(call.result_index, path, call.output))


def parse_claude(events: list[dict]) -> Trace:
    trace = Trace()
    calls = {}
    for index, event in enumerate(events):
        position = index*1000
        message = event.get('message', {})
        if event.get('type') == 'system' and event.get('subtype') == 'init':
            trace.models.append(event.get('model', ''))
            trace.cwd = event.get('cwd', '')
        if event.get('type') == 'assistant':
            for offset, part in enumerate(message.get('content', [])):
                if part.get('type') == 'text':
                    trace.decisions.append(Decision(position+offset, message.get('id', ''), part.get('text', '')))
                if part.get('type') == 'tool_use' and part['id'] not in calls:
                    args = part.get('input', {})
                    call = ToolCall(part['id'], message.get('id', ''), part.get('name', ''),
                                    position+offset, arguments=args, command=args.get('command', ''))
                    calls[call.identifier] = call
                    trace.calls.append(call)
        if event.get('type') != 'user':
            continue
        context = content_text(message.get('content', []))
        if context:
            trace.evidence.append(Evidence(position, context, context))
        for part in message.get('content', []):
            if part.get('type') != 'tool_result' or part.get('tool_use_id') not in calls:
                continue
            call = calls[part['tool_use_id']]
            detail = event.get('tool_use_result', {})
            call.output = content_text(part.get('content', ''))
            if isinstance(detail, dict):
                call.output += '\n'+detail.get('stdout', '')+'\n'+detail.get('stderr', '')
            call.result_index = position
            call.error = bool(part.get('is_error'))
            call.exit_code = reported_status(call.output)
            if call.exit_code is None and not call.error:
                call.exit_code = 0
            attach_evidence(trace, call)
    return trace


def parse_pi(events: list[dict]) -> Trace:
    trace = Trace()
    calls = {}
    for index, event in enumerate(events):
        message = event.get('message', {})
        if event.get('type') == 'message_end' and message.get('role') == 'assistant':
            trace.models.append(message.get('model', ''))
            for offset, part in enumerate(message.get('content', [])):
                response = message.get('responseId', '')
                if part.get('type') == 'text':
                    trace.decisions.append(Decision(index*1000+offset, response, part.get('text', '')))
                if part.get('type') == 'toolCall':
                    args = part.get('arguments', {})
                    call = ToolCall(part['id'], response, part.get('name', ''), index*1000+offset,
                                    arguments=args, command=args.get('command', ''))
                    calls[call.identifier] = call
                    trace.calls.append(call)
        if event.get('type') == 'tool_execution_end' and event.get('toolCallId') in calls:
            call = calls[event['toolCallId']]
            result = event.get('result', {})
            call.result_index = index*1000
            call.output = content_text(result.get('content', []))
            call.error = bool(event.get('isError'))
            call.exit_code = result.get('details', {}).get('exitCode', reported_status(call.output))
            if call.exit_code is None and not call.error:
                call.exit_code = 0
            attach_evidence(trace, call)
    return trace


def parse_opencode(events: list[dict], models: list[dict]) -> Trace:
    trace = Trace(models=[m['providerID']+'/'+m['modelID'] for m in models
                          if m.get('providerID') and m.get('modelID')])
    starts = {}
    for index, event in enumerate(events):
        part = event.get('part', {})
        response = part.get('messageID', '')
        if event.get('type') == 'step_start':
            starts[response] = index*1000
        if event.get('type') == 'text':
            trace.decisions.append(Decision(index*1000, response, part.get('text', '')))
        if event.get('type') != 'tool_use':
            continue
        state = part.get('state', {})
        args = state.get('input', {})
        call = ToolCall(part.get('id', str(index)), response, part.get('tool', ''),
                        starts.get(response, index*1000), index*1000, arguments=args,
                        command=args.get('command', ''), output=state.get('output', ''),
                        exit_code=state.get('metadata', {}).get('exit'), error=state.get('status') != 'completed')
        trace.calls.append(call)
        attach_evidence(trace, call)
        metadata = state.get('metadata', {})
        if call.tool == 'skill' and metadata.get('dir'):
            trace.evidence.append(Evidence(call.result_index, str(Path(metadata['dir'])/'SKILL.md'), call.output))
        display = metadata.get('display', {})
        if (call.tool == 'read' and display.get('truncated') is False
                and display.get('lineStart') == 1 and display.get('lineEnd') == display.get('totalLines')):
            trace.evidence.append(Evidence(call.result_index, args.get('filePath', ''), display.get('text', '')))
    return trace


def literal_commands(code: str) -> tuple[list[str], int]:
    count = len(re.findall(r'\btools\.\w+\s*\(', code))
    pattern = r'\b(?:cmd|command)\s*:\s*("(?:\\.|[^"\\])*"|\x27(?:\\.|[^\x27\\])*\x27)'
    commands = [ast.literal_eval(m[1]) for m in re.finditer(pattern, code)]
    return commands, count or 1


def codex_request(payload: dict, position: int, response: str) -> ToolCall:
    code = payload.get('input', payload.get('arguments', ''))
    if payload['type'] == 'function_call':
        args = json.loads(code)
        return ToolCall(payload['call_id'], response, payload.get('name', ''), position,
                        arguments=args, command=args.get('cmd', args.get('command', '')))
    commands, count = literal_commands(code)
    if 'tools.exec_command(' in code and not commands:
        raise ValueError('Computed Codex command needs manual review')
    args = {'recorded_code': code, 'recorded_commands': commands}
    directory = re.search(r'\bworkdir\s*:\s*("(?:\\.|[^"\\])*"|\x27(?:\\.|[^\x27\\])*\x27)', code)
    if directory:
        args['workdir'] = ast.literal_eval(directory[1])
    elif re.search(r'\bworkdir\s*:', code):
        raise ValueError('Computed Codex working directory needs manual review')
    return ToolCall(payload['call_id'], response, payload.get('name', ''), position,
                    command=commands[0] if commands else '', arguments=args, weight=count)


def parse_codex(events: list[dict], native: list[dict]) -> Trace:
    trace = Trace()
    if not native:
        trace.errors.append('Missing owned Codex native rollout; response boundaries and runtime model are unverified')
        return trace
    shell_results = [e['item'] for e in events if e.get('type') == 'item.completed'
                     and e.get('item', {}).get('type') == 'command_execution']
    calls = {}
    generation = 0
    for index, event in enumerate(native):
        payload = event.get('payload', {})
        if event.get('type') == 'turn_context':
            trace.models.append(payload.get('model', ''))
        if event.get('type') == 'event_msg' and payload.get('type') == 'token_count':
            generation += 1
        response = f'generation-{generation}'
        if event.get('type') == 'event_msg' and payload.get('type') == 'agent_message':
            trace.decisions.append(Decision(index*1000, response, payload.get('message', '')))
        if event.get('type') != 'response_item':
            continue
        if payload.get('type') == 'message':
            context = content_text(payload.get('content', []))
            trace.evidence.append(Evidence(index*1000, context, context))
        if payload.get('type') in ('custom_tool_call', 'function_call'):
            try:
                call = codex_request(payload, index*1000, response)
            except (ValueError, SyntaxError) as error:
                trace.errors.append(str(error))
                call = ToolCall(payload['call_id'], response, payload.get('name', ''), index*1000)
            calls[call.identifier] = call
            trace.calls.append(call)
        if payload.get('type') in ('custom_tool_call_output', 'function_call_output') and payload.get('call_id') in calls:
            call = calls[payload['call_id']]
            call.result_index = index*1000
            output = payload.get('output', '')
            call.output = content_text(output) if isinstance(output, list) else str(output)
            for item in shell_results:
                if call.command and unwrap_host_shell(item.get('command', '')) == call.command:
                    call.exit_code = item.get('exit_code')
                    call.output += '\n'+item.get('aggregated_output', '')
                    shell_results.remove(item)
                    break
            if call.exit_code is None:
                match = re.search(r'"exit_code"\s*:\s*(-?\d+)', call.output)
                call.exit_code = int(match[1]) if match else reported_status(call.output)
            attach_evidence(trace, call)
    return trace


def parse_events(agent: str, events: list[dict], *, native: list[dict] | None = None,
                 models: list[dict] | None = None) -> Trace:
    parsers = {'claude': lambda: parse_claude(events), 'pi': lambda: parse_pi(events),
               'opencode': lambda: parse_opencode(events, models or []),
               'codex': lambda: parse_codex(events, native or [])}
    try:
        trace = parsers[agent]()
    except (KeyError, ValueError, TypeError, AttributeError) as error:
        trace = Trace(errors=[f'Unsupported {agent} native schema: {type(error).__name__}'])
    trace.models = sorted(set(m for m in trace.models if m))
    if not trace.models:
        trace.errors.append('Observed runtime model identifier is missing')
    return trace
