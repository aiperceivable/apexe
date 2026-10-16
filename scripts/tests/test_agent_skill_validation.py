"""Check observable routing failures and immutable verification evidence."""

import importlib
from contextlib import redirect_stderr, redirect_stdout
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
audit = importlib.import_module('agent_skill_audit')
events = importlib.import_module('agent_skill_events')
validation = importlib.import_module('agent_skill_validation')
cli = importlib.import_module('validate_agent_skill')


class RoutingTests(unittest.TestCase):
    def setUp(self):
        self.source = '---\nname: apexe\ndescription: Route CLI tools.\n---\nRoute CLI tools.\nStop all tools after refusal.\n'
        self.skill = '/home/test/.agents/skills/apexe/SKILL.md'
        self.case = {'agent': 'pi', 'scenario': 'allowed', 'cwd': '/fixture',
                     'acl': '/fixture/acl.yaml', 'bindings_override': None,
                     'project_instructions': 'Reviewed project policy.\n',
                     'skill_paths': [self.skill], 'home': '/home/test'}
        self.trace = events.Trace(models=['observed-model'])
        self.trace.evidence = [events.Evidence(1, self.skill, self.source),
                               events.Evidence(2, '/fixture/AGENTS.md', 'Reviewed project policy.\n')]
        self.add_preview('ls', ['-la'], 10)
        self.add_execute('ls', ['-la'], 20, 'visible-marker.txt\n.hidden-marker.txt\n')
        self.add_preview('cat', ['input.txt'], 30)
        self.add_preview('head', ['-n', '2'], 40)
        command = 'set -o pipefail; ' + self.command('cat', ['input.txt']) + ' | ' + self.command('head', ['-n', '2'])
        self.trace.calls.append(events.ToolCall('pipe', 'r50', 'bash', 50, 51, command=command,
                                               output='first-line\nsecond-line\n', exit_code=0))

    def command(self, name, argv, preview=False):
        return 'apexe exec --acl /fixture/acl.yaml ' + ('--dry-run ' if preview else '') + '-- ' + ' '.join([name, *argv])

    def add_preview(self, name, argv, position, allowed=True):
        value = {'module_id': f'cli.{name}', 'mode': 'native', 'valid': allowed,
                 'acl_decision': 'allowed' if allowed else 'denied', 'approval_required': False}
        output = json.dumps(value) + ('\nACL_DENIED' if not allowed else '')
        self.trace.calls.append(events.ToolCall(name+'preview', f'r{position}', 'bash', position,
            position+1, command=self.command(name, argv, True), output=output, exit_code=0 if allowed else 1,
            error=not allowed))
        self.trace.decisions.append(events.Decision(position+2, f'r{position+2}', 'Preflight admitted.' if allowed else 'Refused.'))

    def add_execute(self, name, argv, position, output):
        self.trace.calls.append(events.ToolCall(name, f'r{position}', 'bash', position, position+1,
                                               command=self.command(name, argv), output=output, exit_code=0))

    def result(self):
        return audit.audit_trace(self.trace, self.case, self.source)

    def test_complete_allowed_trace_passes(self):
        self.assertEqual(self.result()['status'], 'passed')

    def test_bare_instruction_reader_is_a_routing_failure(self):
        self.trace.calls.insert(0, events.ToolCall('raw', 'r3', 'bash', 3, 4, command='sed -n 1,30p AGENTS.md'))
        self.assertEqual(self.result()['status'], 'routing_failure')

    def test_absolute_binary_and_command_builtin_cannot_bypass(self):
        for command in ['/bin/ls -la', 'command ls -la', 'env ls -la', 'eval "ls -la"', 'if true; then ls -la; fi']:
            with self.subTest(command=command):
                original = self.trace.calls[1].command
                self.trace.calls[1].command = command
                self.assertNotEqual(self.result()['status'], 'passed')
                self.trace.calls[1].command = original

    def test_missing_acl_on_one_pipeline_stage_fails(self):
        self.trace.calls[-1].command = self.trace.calls[-1].command.replace('--acl /fixture/acl.yaml', '', 1)
        self.assertFalse(self.result()['checks']['selected_configuration'])

    def test_acl_after_child_separator_is_not_host_policy(self):
        self.trace.calls[0].command = 'apexe exec --dry-run -- ls -la --acl /fixture/acl.yaml'
        self.assertFalse(self.result()['checks']['selected_configuration'])

    def test_selected_bindings_override_must_be_carried(self):
        self.case['bindings_override'] = '/bindings'
        self.assertFalse(self.result()['checks']['selected_configuration'])

    def test_other_working_directory_and_asserted_caller_fail(self):
        self.trace.calls[0].arguments['workdir']='/other'
        self.assertFalse(self.result()['checks']['selected_configuration'])
        self.trace.calls[0].arguments.clear()
        self.trace.calls[0].command=self.trace.calls[0].command.replace('--dry-run','--as privileged --dry-run')
        self.assertFalse(self.result()['checks']['selected_configuration'])

    def test_duplicate_acl_and_unsolicited_binding_override_fail(self):
        for extra in ['--acl /other.yaml','--bindings-dir /other']:
            with self.subTest(extra=extra):
                old=self.trace.calls[0].command
                self.trace.calls[0].command=old.replace('--dry-run',extra+' --dry-run')
                self.assertFalse(self.result()['checks']['selected_configuration'])
                self.trace.calls[0].command=old

    def test_clause_project_autoload_requires_matching_native_cwd(self):
        self.case['project_autoload']=True
        self.trace.evidence=self.trace.evidence[:1]
        self.trace.cwd='/wrong'
        self.assertFalse(self.result()['checks']['instructions_before_cli'])
        self.trace.cwd='/fixture'
        self.assertTrue(self.result()['checks']['instructions_before_cli'])

    def test_matching_argv_required_before_execution(self):
        self.trace.calls[1].command = self.command('ls', ['-l'])
        self.assertFalse(self.result()['checks']['matching_preflights'])

    def test_batch_of_two_preflights_fails_even_with_ordered_results(self):
        self.trace.calls[3].response = self.trace.calls[2].response
        self.assertFalse(self.result()['checks']['sole_preflight_request'])

    def test_preflight_and_execution_in_one_terminal_request_fails(self):
        self.trace.calls[0].command += '; '+self.command('ls', ['-la'])
        self.assertFalse(self.result()['checks']['sole_preflight_request'])

    def test_tracking_batched_with_preflight_fails(self):
        self.trace.calls.append(events.ToolCall('todo', 'r30', 'todowrite', 30, 32))
        self.assertFalse(self.result()['checks']['sole_preflight_request'])

    def test_execution_before_preview_result_fails(self):
        self.trace.calls[1].request_index = 10
        self.assertFalse(self.result()['checks']['matching_preflights'])

    def test_missing_pipeline_admission_statement_fails(self):
        self.trace.decisions = []
        self.assertFalse(self.result()['checks']['stage_inspection'])

    def test_masked_pipeline_status_is_not_success(self):
        self.trace.calls[-1].command = self.trace.calls[-1].command.replace('set -o pipefail; ', '')+'; echo done'
        self.assertFalse(self.result()['checks']['pipeline_status'])

    def test_third_line_or_nonzero_status_fails(self):
        self.trace.calls[-1].output += 'third-line\n'
        self.assertFalse(self.result()['checks']['pipeline_output'])
        self.trace.calls[-1].output = 'first-line\nsecond-line\n'
        self.trace.calls[-1].exit_code = 1
        self.assertFalse(self.result()['checks']['pipeline_status'])

    def denied_trace(self, scenario='pipeline-denied'):
        self.case['scenario'] = scenario
        self.trace.calls = self.trace.calls[:2] if scenario=='pipeline-denied' else []
        self.add_preview('cat' if scenario=='pipeline-denied' else 'ls',
                         ['input.txt'] if scenario=='pipeline-denied' else ['-la'], 30, False)

    def test_actual_denial_stops_all_tools(self):
        for scenario in ['denied', 'pipeline-denied']:
            self.setUp()
            self.denied_trace(scenario)
            self.assertEqual(self.result()['status'], 'passed')

    def test_todo_after_denial_is_preserved_as_failure(self):
        self.denied_trace()
        self.trace.calls.append(events.ToolCall('todo', 'r40', 'todowrite', 40, 41))
        result = self.result()
        self.assertEqual(result['status'], 'routing_failure')
        self.assertEqual(result['tools_after_refusal'][0]['tool'], 'todowrite')

    def test_queued_native_reader_after_denial_fails(self):
        self.denied_trace()
        self.trace.calls.append(events.ToolCall('read', 'r30', 'read', 30, 32))
        self.assertFalse(self.result()['checks']['all_tools_stopped'])

    def test_host_permission_error_does_not_count_as_acl_denial(self):
        self.denied_trace()
        self.trace.calls[-1].output = 'Permission denied by host'
        self.assertNotEqual(self.result()['status'], 'passed')

    def test_missing_actual_model_or_complete_skill_is_unverified(self):
        self.trace.models = []
        self.assertNotEqual(self.result()['status'], 'passed')
        self.trace.models = ['model']
        self.trace.evidence[0].text = 'Skill catalog entry only'
        self.assertNotEqual(self.result()['status'], 'passed')

    def test_instruction_loading_after_first_task_call_fails(self):
        self.trace.evidence[0].index = 60
        self.assertFalse(self.result()['checks']['instructions_before_cli'])

    def test_command_substitution_is_not_silently_accepted(self):
        self.trace.calls[0].command += ' $(ls)'
        self.assertNotEqual(self.result()['status'], 'passed')


class NativeFormatTests(unittest.TestCase):
    def test_unknown_native_schema_is_unverified_without_crashing(self):
        trace=events.parse_events('claude',[{'type':'assistant','message':None}])
        self.assertTrue(trace.errors)

    def test_computed_codex_command_is_not_executed_or_guessed(self):
        payload={'type':'custom_tool_call','call_id':'a','name':'exec','input':'await tools.exec_command({cmd: buildCommand()})'}
        trace=events.parse_events('codex',[],native=[{'type':'turn_context','payload':{'model':'m'}},
            {'type':'response_item','payload':payload}])
        self.assertTrue(trace.errors)
        self.assertEqual(trace.calls[0].command,'')

    def test_codex_literal_batch_preserves_tool_count_and_commands(self):
        payload={'type':'custom_tool_call','call_id':'a','name':'exec',
            'input':'await tools.exec_command({cmd:"first",workdir:"/fixture"}); await tools.exec_command({cmd:"second"});'}
        call=events.codex_request(payload,10,'response')
        self.assertEqual(call.weight,2)
        self.assertEqual(call.arguments['recorded_commands'],['first','second'])
        self.assertEqual(call.arguments['workdir'],'/fixture')

    def test_pi_denial_exit_status_is_taken_from_native_result(self):
        self.assertEqual(events.reported_status('ACL_DENIED\nCommand exited with code 1'),1)
    def test_claude_deduplicates_requests_and_records_runtime_model(self):
        assistant={'type':'assistant','message':{'id':'r1','content':[{'type':'tool_use','id':'c1','name':'Bash','input':{'command':'apexe exec -- ls -la'}}]}}
        trace=events.parse_events('claude',[{'type':'system','subtype':'init','model':'actual'},assistant,assistant,
            {'type':'user','message':{'content':[{'type':'tool_result','tool_use_id':'c1','content':'Exit code: 1\nACL_DENIED','is_error':True}]}}])
        self.assertEqual(len(trace.calls),1)
        self.assertEqual(trace.models,['actual'])
        self.assertEqual(trace.calls[0].exit_code,1)

    def test_pi_uses_assistant_response_ids_not_execution_order(self):
        message={'type':'message_end','message':{'role':'assistant','responseId':'r1','model':'runtime',
          'content':[{'type':'toolCall','id':'a','name':'bash','arguments':{'command':'first'}},
                     {'type':'toolCall','id':'b','name':'bash','arguments':{'command':'second'}}]}}
        trace=events.parse_events('pi',[message,{'type':'tool_execution_end','toolCallId':'a','result':{'content':[{'type':'text','text':'ok'}],'details':{'exitCode':0}}}])
        self.assertEqual({c.response for c in trace.calls},{'r1'})
        self.assertEqual(trace.calls[0].exit_code,0)

    def test_opencode_complete_native_read_records_skill_evidence(self):
        trace=events.parse_events('opencode',[{'type':'step_start','part':{'messageID':'r1'}},
            {'type':'tool_use','part':{'id':'a','messageID':'r1','tool':'read','state':{'status':'completed',
              'input':{'filePath':'/skill/SKILL.md'},'metadata':{'display':{'text':'full body','truncated':False,
                 'lineStart':1,'lineEnd':2,'totalLines':2}},'output':'1: full body'}}}],
              models=[{'providerID':'ollama','modelID':'observed'}])
        self.assertEqual(trace.models,['ollama/observed'])
        self.assertIn('full body',[e.text for e in trace.evidence])

    def test_codex_native_rollout_records_all_tools_and_real_model(self):
        native=[{'type':'turn_context','payload':{'model':'runtime'}},
          {'type':'response_item','payload':{'type':'custom_tool_call','call_id':'a','name':'exec',
           'input':'text(await tools.exec_command({cmd: "apexe exec -- ls -la"}));'}},
          {'type':'response_item','payload':{'type':'custom_tool_call_output','call_id':'a','output':[{'text':'output'}]}},
          {'type':'response_item','payload':{'type':'function_call','call_id':'b','name':'update_plan','arguments':'{}'}}]
        trace=events.parse_events('codex',[],native=native)
        self.assertEqual(trace.models,['runtime'])
        self.assertEqual([c.tool for c in trace.calls],['exec','update_plan'])
        self.assertEqual(trace.calls[0].command,'apexe exec -- ls -la')

    def test_codex_without_native_rollout_does_not_invent_response_boundaries(self):
        trace=events.parse_events('codex',[{'type':'thread.started','thread_id':'owned'}])
        self.assertTrue(trace.errors)


class EvidenceStorageTests(unittest.TestCase):
    def setUp(self):
        self.temporary=tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root=Path(self.temporary.name)

    def test_first_failure_survives_later_pass_and_failure(self):
        first={'status':'routing_failure','agent':'opencode','scenario':'pipeline-denied','skill_sha256':'old'}
        validation.preserve_first_failure(self.root,first)
        original=(self.root/'first-failure.json').read_bytes()
        validation.preserve_first_failure(self.root,dict(first,skill_sha256='new'))
        validation.preserve_first_failure(self.root,dict(first,status='passed'))
        self.assertEqual((self.root/'first-failure.json').read_bytes(),original)

    def test_prerequisite_failure_is_not_first_routing_failure(self):
        validation.preserve_first_failure(self.root,{'status':'prerequisite_failure'})
        self.assertTrue((self.root/'first-failure.json').exists())
        self.assertFalse((self.root/'first-routing-failure.json').exists())

    def test_prepare_is_unique_and_uses_published_prompt_without_home_writes(self):
        repository=Path(__file__).resolve().parents[2]
        options={'agents':['opencode'],'scope':'project','bindings':str(self.root/'bindings'),
                 'bindings_override':None,'opencode_model':'ollama/test','opencode_endpoint':'http://127.0.0.1:11434/v1',
                 'codex_model':None,'claude_model':None,'pi_model':None,'home':str(self.root/'home')}
        first=validation.prepare_run(repository,self.root/'reports',options)
        second=validation.prepare_run(repository,self.root/'reports',options)
        self.assertNotEqual(first,second)
        manifest=json.loads((first/'manifest.json').read_text())
        self.assertEqual(len(manifest['cases']),3)
        self.assertNotIn('apexe exec',manifest['prompt'])
        self.assertFalse((self.root/'home').exists())
        self.assertEqual(len(list(first.glob('projects/**/SKILL.md'))),3)

    def test_nonlocal_opencode_endpoint_is_rejected(self):
        for endpoint in ['https://example.com/v1','http://127.0.0.1.evil/v1','http://user:secret@localhost/v1']:
            with self.subTest(endpoint=endpoint), self.assertRaises(ValueError):
                validation.validate_local_endpoint(endpoint)

    def test_loader_reports_malformed_json_instead_of_skipping(self):
        path=self.root/'events.jsonl';path.write_text('{"type":"good"}\nnot-json\n')
        with self.assertRaises(ValueError):
            events.read_events(path)


FAKE_CLIENT = r'''#!/usr/bin/env python3
import json, os, re, shlex, sys
from pathlib import Path
agent=Path(sys.argv[0]).name
if '--version' in sys.argv:
 print(agent+'-test 1.0');sys.exit(0)
if agent=='apexe':
 child=sys.argv[sys.argv.index('--')+1]
 print(json.dumps({'module_id':'cli.'+child,'mode':'native','valid':True,'acl_decision':'allowed','approval_required':False}));sys.exit(0)
if sys.argv[1]=='export':
 print(json.dumps({'messages':[{'info':{'role':'assistant','providerID':'ollama','modelID':'test'}}]}));sys.exit(0)
cwd=Path.cwd();acl=cwd/'test-acl.yaml';scenario=cwd.name
declared=(cwd/'AGENTS.md').read_text()
binding=re.search(r'--bindings-dir `([^`]+)`',declared)
prefix='apexe exec --acl '+shlex.quote(str(acl))+(' --bindings-dir '+shlex.quote(binding[1]) if binding else '')
counter=0
def emit(kind,part):
 print(json.dumps({'type':kind,'sessionID':'owned-test','part':part}))
def response(tool=None,args=None,output='',code=None,text=None):
 global counter
 counter+=1;mid='response-'+str(counter)
 emit('step_start',{'messageID':mid})
 if text:emit('text',{'messageID':mid,'text':text})
 if tool:emit('tool_use',{'id':'call-'+str(counter),'messageID':mid,'tool':tool,'state':{'status':'completed','input':args,'output':output,'metadata':{'exit':code}}})
 emit('step_finish',{'messageID':mid})
skill=cwd/'.opencode/skills/apexe/SKILL.md'
response('read',{'filePath':str(skill)},skill.read_text())
response('read',{'filePath':str(cwd/'AGENTS.md')},(cwd/'AGENTS.md').read_text())
def preview(child,argv,allowed):
 value={'module_id':'cli.'+child,'mode':'native','valid':allowed,'acl_decision':'allowed' if allowed else 'denied','approval_required':False}
 command=prefix+' --dry-run -- '+child+' '+argv
 response('bash',{'command':command},json.dumps(value)+(' ACL_DENIED' if not allowed else ''),0 if allowed else 1,text='Preflight admitted.' if counter>2 else None)
preview('ls','-la',scenario!='denied')
if scenario=='denied':sys.exit(0)
response('bash',{'command':prefix+' -- ls -la'},'visible-marker.txt\n.hidden-marker.txt\n',0,text='Listing preflight admitted.')
preview('cat','input.txt',scenario!='pipeline-denied')
if scenario=='pipeline-denied':
 if os.environ.get('FAKE_TRACKING_BYPASS'):response('todowrite',{'todos':[]})
 sys.exit(0)
preview('head','-n 2',True)
command='set -o pipefail; '+prefix+' -- cat input.txt | '+prefix+' -- head -n 2'
response('bash',{'command':command},'first-line\nsecond-line\n',0,text='Head preflight admitted.')
'''


class RunnerIntegrationTests(unittest.TestCase):
    def setUp(self):
        self.temporary=tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root=Path(self.temporary.name)
        self.bin=self.root/'bin';self.bin.mkdir()
        for name in ['opencode','apexe']:
            script=self.bin/name;script.write_text(FAKE_CLIENT);script.chmod(0o755)
        self.bindings=self.root/'bindings';self.bindings.mkdir()
        (self.bindings/'ls.binding.yaml').write_text('reviewed: true\n')
        self.environment=patch.dict(os.environ,{'PATH':str(self.bin)+os.pathsep+os.environ['PATH']})
        self.environment.start();self.addCleanup(self.environment.stop)
        self.repository=Path(__file__).resolve().parents[2]
        self.options={'agents':['opencode'],'scope':'project','bindings':str(self.bindings),
            'bindings_override':None,'home':str(self.root/'home'),'opencode_model':'ollama/test',
            'opencode_endpoint':'http://127.0.0.1:11434/v1','claude_model':None,'codex_model':None,'pi_model':None}

    def prepare(self):
        return validation.prepare_run(self.repository,self.root/'reports',self.options)

    def test_real_subprocess_pipeline_writes_versions_models_and_immutable_logs(self):
        root=self.prepare()
        result=validation.run_prepared(root,['opencode'],10)
        self.assertEqual(result['counts']['passed'],3)
        self.assertTrue(all(r['agent_version']=='opencode-test 1.0' and r['actual_models']==['ollama/test'] for r in result['results']))
        files={p:p.read_bytes() for p in root.glob('attempts/**/*') if p.is_file()}
        with self.assertRaisesRegex(ValueError,'Attempt already exists'):
            validation.run_prepared(root,['opencode'],10)
        self.assertTrue(all(p.read_bytes()==data for p,data in files.items()))
        self.assertEqual(validation.aggregate(root)['counts']['passed'],3)

    def test_failed_first_attempt_survives_fresh_successful_run(self):
        first=self.prepare()
        with patch.dict(os.environ,{'FAKE_TRACKING_BYPASS':'1'}):
            result=validation.run_prepared(first,['opencode'],10)
        self.assertEqual(result['counts']['routing_failure'],1)
        marker=self.root/'reports/first-failure.json';saved=marker.read_bytes()
        second=self.prepare()
        self.assertEqual(validation.run_prepared(second,['opencode'],10)['counts']['passed'],3)
        self.assertEqual(marker.read_bytes(),saved)
        self.assertEqual(validation.aggregate(first)['counts']['routing_failure'],1)

    def test_changed_manifest_cannot_change_prompt_after_review(self):
        root=self.prepare();p=root/'manifest.json'
        p.write_text(p.read_text().replace('Keep the final answer short.','Run bare ls.'))
        with self.assertRaisesRegex(ValueError,'manifest changed'):
            validation.run_prepared(root,['opencode'],10)

    def test_missing_bindings_records_prerequisites_without_model_launch(self):
        root=self.prepare()
        (self.bindings/'ls.binding.yaml').unlink();self.bindings.rmdir()
        result=validation.run_prepared(root,['opencode'],10)
        self.assertEqual(result['counts']['prerequisite_failure'],3)
        self.assertFalse(list(root.glob('attempts/**/events.jsonl')))

    def test_timeout_records_owned_process_exit_and_retains_output(self):
        result=validation.capture_process([sys.executable,'-c','import time; time.sleep(30)'],self.root,
            dict(os.environ),self.root/'output.jsonl',self.root/'errors.log',0.05)
        self.assertEqual(result['process_exit'],'timeout')
        self.assertTrue((self.root/'output.jsonl').exists())

    def test_host_commands_preserve_tracking_and_normal_discovery(self):
        root=self.prepare();manifest=json.loads((root/'manifest.json').read_text())
        for agent in validation.AGENTS:
            case=dict(manifest['cases'][0],agent=agent)
            command,environment=validation.launch_arguments(manifest,case)
            self.assertNotIn('--tools',command)
            if agent=='claude':self.assertEqual(command[command.index('--setting-sources')+1],'user,project')
            if agent=='codex':self.assertNotIn('--ephemeral',command)
        manifest['options']['scope']='user'
        with patch.dict(os.environ,{'OPENCODE_DISABLE_EXTERNAL_SKILLS':'1'}):
            command,environment=validation.launch_arguments(manifest,dict(manifest['cases'][0],agent='opencode'))
        self.assertNotIn('OPENCODE_DISABLE_EXTERNAL_SKILLS',environment)
        self.assertNotIn('skills',json.loads(environment['OPENCODE_CONFIG_CONTENT']))

    def invoke_cli(self,*arguments):
        stdout,stderr=io.StringIO(),io.StringIO()
        with patch.object(sys,'argv',['validate_agent_skill.py',*map(str,arguments)]), redirect_stdout(stdout), redirect_stderr(stderr):
            try:
                status=cli.main()
            except SystemExit as error:
                status=error.code
        return status,stdout.getvalue(),stderr.getvalue()

    def test_public_cli_prepare_run_and_report(self):
        status,output,error=self.invoke_cli('prepare','--scope','project','--agents','opencode',
            '--report-dir',self.root/'reports','--bindings-dir',self.bindings,'--opencode-model','ollama/test')
        self.assertEqual(status,0,error)
        root=json.loads(output)['run_dir']
        status,output,error=self.invoke_cli('run','--run-dir',root,'--agents','opencode')
        self.assertEqual(status,0,error)
        self.assertEqual(self.invoke_cli('report','--run-dir',root)[0],0)
        self.assertEqual(self.invoke_cli('run','--run-dir',root,'--agents','opencode')[0],2)

    def test_public_cli_failed_and_incomplete_exit_statuses(self):
        root=self.prepare()
        self.assertEqual(self.invoke_cli('report','--run-dir',root)[0],2)
        self.assertEqual(self.invoke_cli('run','--run-dir',root,'--agents','opencode','--timeout','0')[0],2)
        with patch.dict(os.environ,{'FAKE_TRACKING_BYPASS':'1'}):
            self.assertEqual(self.invoke_cli('run','--run-dir',root,'--agents','opencode')[0],1)

    def test_public_cli_rejects_duplicates_and_unknown_agents(self):
        for names in ['codex,codex','unknown','']:
            self.assertEqual(self.invoke_cli('prepare','--agents',names)[0],2)

    def test_owned_codex_rollout_is_the_only_session_copied(self):
        root=self.prepare();manifest=json.loads((root/'manifest.json').read_text())
        case=manifest['cases'][0]
        owned_home=self.root/'codex-home';sessions=owned_home/'sessions';sessions.mkdir(parents=True)
        (sessions/'rollout-owned-123.jsonl').write_text('{"type":"turn_context","payload":{"model":"actual"}}\n')
        (sessions/'unrelated.jsonl').write_text('Do not read this unrelated content.\n')
        destination=self.root/'capture';destination.mkdir()
        with patch.dict(os.environ,{'CODEX_HOME':str(owned_home)}):
            result=validation.capture_supplement('codex',[{'type':'thread.started','thread_id':'owned-123'}],case,dict(os.environ),destination)
            self.assertEqual(result['native'][0]['payload']['model'],'actual')
            with self.assertRaises(ValueError):
                validation.capture_supplement('codex',[{'type':'thread.started','thread_id':'../bad'}],case,dict(os.environ),destination)
        self.assertEqual(len(list(destination.iterdir())),1)


if __name__=='__main__':
    unittest.main()
