#!/usr/bin/env python3
"""Prepare private fixtures and verify native apexe routing through four Agents."""

import argparse
import json
import os
from pathlib import Path

from agent_skill_validation import AGENTS, aggregate, prepare_run, run_prepared


def selected_agents(value: str) -> list[str]:
    names = list(AGENTS) if value == 'all' else value.split(',')
    if not names or len(set(names)) != len(names) or any(name not in AGENTS for name in names):
        raise argparse.ArgumentTypeError('Use all or distinct comma-separated claude,codex,pi,opencode names')
    return names


def main() -> int:
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='action', required=True)
    prepare = commands.add_parser('prepare', help='Create a fresh run and review.md without calling models')
    prepare.add_argument('--report-dir', type=Path, default=Path('target/agent-skill-verification'))
    prepare.add_argument('--agents', type=selected_agents, default=list(AGENTS))
    prepare.add_argument('--scope', choices=['project', 'user'], default='project')
    prepare.add_argument('--bindings-dir', type=Path, help='Explicit reviewed binding override; propagated on every exec')
    prepare.add_argument('--opencode-model', default='ollama/qwen3.8-code-64k')
    prepare.add_argument('--opencode-endpoint', default='http://127.0.0.1:11434/v1')
    for agent in ('claude', 'codex', 'pi'):
        prepare.add_argument('--'+agent+'-model', help='Optional requested model; actual model is taken from native events')
    run = commands.add_parser('run', help='Run selected prepared Agents once; never overwrite an attempt')
    run.add_argument('--run-dir', type=Path, required=True)
    run.add_argument('--agents', type=selected_agents, default=list(AGENTS))
    run.add_argument('--timeout', type=float, default=1200, help='Maximum seconds for each scenario')
    report = commands.add_parser('report', help='Rebuild the summary from saved per-scenario receipts without model calls')
    report.add_argument('--run-dir', type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.action == 'prepare':
            home = Path.home().resolve()
            bindings = (args.bindings_dir or home/'.apexe/bindings').expanduser().resolve()
            options = {'agents': args.agents, 'scope': args.scope, 'home': str(home),
                       'bindings': str(bindings), 'bindings_override': str(bindings),
                       **{agent+'_model': getattr(args, agent+'_model') for agent in AGENTS},
                       'opencode_endpoint': args.opencode_endpoint}
            root = prepare_run(Path(__file__).resolve().parents[1], args.report_dir, options)
            print(json.dumps({'run_dir': str(root), 'review': str(root/'review.md')}))
            return 0
        root = args.run_dir.expanduser().resolve()
        if args.action == 'run':
            if args.timeout <= 0:
                raise ValueError('Timeout must be positive')
            receipt = run_prepared(root, args.agents, args.timeout)
        else:
            receipt = aggregate(root)
        print(json.dumps({'run_dir': str(root), 'completed': receipt['completed'],
                          'expected': receipt['expected'], 'counts': receipt['counts']}))
        if receipt['counts']['routing_failure']:
            return 1
        return 0 if receipt['completed'] == receipt['expected'] and not any(
            receipt['counts'][status] for status in ('prerequisite_failure', 'unverified')) else 2
    except (OSError, ValueError, KeyError) as error:
        parser.exit(2, f'Error: {error}\n')


if __name__ == '__main__':
    raise SystemExit(main())
