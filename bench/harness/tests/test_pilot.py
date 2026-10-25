import copy
import json
from pathlib import Path

import pytest

from bench.harness.experiments.task_quality import assess, review_template, TASKS
from bench.harness.experiments.pilot import (
    EXECUTION_SOURCES, accounting_valid, audit_wire, validate_pilot, require_technical_smoke,
)
from bench.harness.experiments.navigation_skill import (
    digest, schedule, load_reviews, validate_smoke,
)
from bench.harness.experiments.report import summaries


def reviewed(task='symbol_cli_config_flow'):
    r = review_template(task)
    r['review_complete'] = True
    for dimension in ('components', 'relationships'):
        for item in r[dimension]:
            item.update(score=1, evidence='snapshot/file.rs:1 verified owner and relationship')
    return r


def test_task_rubric_separates_incidental_errors_from_task_errors():
    task='symbol_cli_config_flow'
    r=reviewed()
    r['incidental_errors']=[{'claim':'wrong optional implementation detail', 'evidence':'snapshot/file.rs:2'}]
    assert assess(task,r)['task_quality_score']==1
    r['unsupported_symbols']=[{'claim':'optional invented helper', 'evidence':'absent from snapshot', 'scope':'optional'}]
    assert assess(task,r)['task_quality_score']==1
    r['unsupported_symbols'][0]['scope']='required'
    assert assess(task,r)['task_quality_score']==0.5
    r['unsupported_symbols']=[]
    r['task_errors']=[{'claim':'wrong required handoff', 'evidence':'snapshot/file.rs:4'}]
    assert assess(task,r)['task_quality_score']==0.5


def test_quality_missing_review_and_missing_checks_are_not_success():
    assert assess('symbol_cli_config_flow',None)['task_quality_score'] is None
    r=reviewed();r['relationships'].pop()
    with pytest.raises(ValueError,match='preregistered'):
        assess('symbol_cli_config_flow',r)
    r=reviewed();r['components'][0]['evidence']=''
    with pytest.raises(ValueError,match='evidence'):
        assess('symbol_cli_config_flow',r)


def test_strict_and_task_success_are_separate_and_old_reviews_unmodified(tmp_path):
    rows=[{'task_id':'symbol_cli_config_flow','configuration':'B','run_number':1,'status':'completed'}]
    review={'symbol_cli_config_flow/B/1':{'correctness_score':0.5,'evidence':'source contradiction','task_quality':reviewed()}}
    path=tmp_path/'reviews.json';path.write_text(json.dumps(review));original=path.read_bytes()
    load_reviews(tmp_path,rows)
    assert rows[0]['task_success'] is False and rows[0]['task_quality_success'] is True
    assert rows[0]['correctness_score']==0.5 and path.read_bytes()==original
    stats=summaries(rows)['B']
    assert stats['reviewed_successes']==0 and stats['task_oriented_successes']==1
    review['symbol_cli_config_flow/B/1'].pop('task_quality');path.write_text(json.dumps(review))
    load_reviews(tmp_path,rows)
    assert rows[0]['correctness_score']==0.5 and rows[0]['task_quality_score'] is None


def test_pilot_has_fifteen_fresh_rotating_instances():
    order=schedule(TASKS,1,20261018)
    assert len(order)==len(set(order))==15
    assert {x[2] for x in order}=={1}
    firsts={task:next(arm for t,arm,_ in order if t==task) for task in TASKS}
    assert set(firsts.values())==set('ABC')


def test_schedule_supports_repeated_attempts_per_cell():
    order=schedule(TASKS,3,20261018)
    assert len(order)==len(set(order))==45
    assert {x[2] for x in order}=={1,2,3}
    for task in TASKS:
        for run in (1,2,3):
            assert {arm for t,arm,r in order if t==task and r==run}==set('ABC')


def test_summaries_report_convergence_separately_from_exhaustion():
    def row(termination,score,status='completed'):
        return {'configuration':'A','status':status,'task_success':None,'task_quality_success':None,
                'termination_kind':termination,'tool_calls':1,'failed_tool_calls':0,'quality_score':score}
    stats=summaries([row('natural',0.8),row('budget_synthesis',0.0),row('natural',1.0)])['A']
    assert stats['converged_runs']==2 and stats['budget_exhausted_runs']==1
    assert abs(stats['budget_exhaustion_rate']-1/3)<1e-9
    assert abs(stats['converged_only']['quality_score']['mean']-0.9)<1e-9
    assert abs(stats['metrics']['quality_score']['mean']-0.6)<1e-9


def test_grounding_v2_resolves_bare_names_without_double_counting(tmp_path):
    from bench.harness.framework.quality_scorer import MIN_GROUNDED_CITATIONS, QualityScorer
    source=tmp_path/'crates/ignore/src'
    source.mkdir(parents=True)
    (source/'dir.rs').write_text('struct WalkBuilder;\n\nfn matched() {}\n')
    (source/'walk.rs').write_text('fn walk_parallel() {}\n')
    scorer=QualityScorer()
    terse=scorer.score('See crates/ignore/src/dir.rs.',str(tmp_path),'symbol_grounding')
    assert terse.grounded_files_count==1 and terse.citation_count==1
    assert terse.quality_score<1.0,"one citation must not reach a perfect score"
    assert terse.quality_score==terse.grounding_precision/MIN_GROUNDED_CITATIONS
    bare=scorer.score('The logic lives in dir.rs.',str(tmp_path),'symbol_grounding')
    assert bare.grounded_files_count==1 and bare.ambiguous_references_count==0
    both=scorer.score('crates/ignore/src/dir.rs and dir.rs both exist.',str(tmp_path),'symbol_grounding')
    assert both.grounded_files_count==1,"a path and its basename must not count twice"
    rich=scorer.score('crates/ignore/src/dir.rs defines WalkBuilder; walk.rs walks in parallel.',
                      str(tmp_path),'symbol_grounding')
    assert rich.citation_count>=3 and rich.grounding_precision==1.0
    assert rich.score_version=='grounding-v2'


def test_grounding_v2_does_not_credit_ambiguous_bare_names(tmp_path):
    for name in ('a/dir.rs','b/dir.rs'):
        path=tmp_path/name
        path.parent.mkdir(parents=True,exist_ok=True)
        path.write_text('fn same() {}\n')
    from bench.harness.framework.quality_scorer import QualityScorer
    record=QualityScorer().score('The implementation is in dir.rs.',str(tmp_path),'symbol_grounding')
    assert record.grounded_files_count==0,"an ambiguous basename must not be credited"
    assert record.ambiguous_references_count==1


def test_evidence_recall_measures_component_coverage_not_precision(tmp_path):
    import unittest.mock as mock
    from bench.harness.experiments import evidence_anchors
    (tmp_path/'src').mkdir()
    (tmp_path/'src/parse.rs').write_text('struct Parser;\nfn parse_low() {}\n')
    (tmp_path/'src/walk.rs').write_text('struct WalkBuilder;\n')
    fake={'task_x': {'parsing': {'files':['src/parse.rs'], 'symbols':['Parser']},
                     'traversal': {'files':['src/walk.rs'], 'symbols':['WalkBuilder']}}}
    with mock.patch.object(evidence_anchors, 'ANCHORS', fake):
        partial=evidence_anchors.assess('task_x','The flags are handled in src/parse.rs.',str(tmp_path))
        full=evidence_anchors.assess('task_x','src/parse.rs defines Parser; src/walk.rs defines WalkBuilder.',str(tmp_path))
        none=evidence_anchors.assess('task_x','I did not look at the parser.',str(tmp_path))
        unknown=evidence_anchors.assess('task_y','src/parse.rs',str(tmp_path))
    assert partial['evidence_component_matched']==1 and partial['evidence_component_count']==2
    assert partial['evidence_component_recall']==0.5
    assert partial['evidence_anchor_matched']==1 and partial['evidence_anchor_count']==4
    assert partial['evidence_details']['parsing']['covered'] and not partial['evidence_details']['traversal']['covered']
    assert full['evidence_component_recall']==1.0
    assert none['evidence_component_recall']==0.0
    assert unknown['evidence_component_recall'] is None


def test_evidence_anchors_fail_closed_on_stale_definitions(tmp_path):
    import unittest.mock as mock
    from bench.harness.experiments import evidence_anchors
    (tmp_path/'a.rs').write_text('struct Real;\n')
    rubric={'t': {'components': ['c'], 'relationships': []}}
    missing_file={'t': {'c': {'files':['missing.rs'], 'symbols':[]}}}
    missing_symbol={'t': {'c': {'files':[], 'symbols':['AbsentSymbol']}}}
    for broken in (missing_file, missing_symbol):
        with mock.patch.object(evidence_anchors, 'ANCHORS', broken), \
             mock.patch('bench.harness.experiments.task_quality.TASKS', rubric):
            with pytest.raises(ValueError, match='do not match the snapshot'):
                evidence_anchors.verify(str(tmp_path))
    with mock.patch.object(evidence_anchors, 'ANCHORS', {'t': {'c': {'files':['a.rs'], 'symbols':[]}}}), \
         mock.patch('bench.harness.experiments.task_quality.TASKS', rubric):
        with pytest.raises(ValueError, match='No evidence anchors'):
            evidence_anchors.verify(str(tmp_path), ['other'])
        assert evidence_anchors.verify(str(tmp_path))['verified'] is True
        # Anchors and rubric must describe the same components.
        with mock.patch('bench.harness.experiments.task_quality.TASKS',
                        {'t': {'components': ['c', 'not_anchored'], 'relationships': []}}):
            with pytest.raises(ValueError, match='anchors and rubric disagree'):
                evidence_anchors.verify(str(tmp_path))
    from bench.harness.experiments.completion_guidance import CAUSAL_GUIDANCE
    config={'model':'m','temperature':0,'max_output_tokens':4096,'request_options':{},'max_iterations':2}
    tool={'type':'function','function':{'name':'read_file'}}
    base={'model':'m','temperature':0,'max_tokens':4096,'stream':False}
    (tmp_path/'request_01.json').write_text(json.dumps(
        {**base,'messages':[{'role':'system','content':'neutral'},{'role':'user','content':'task'}],'tools':[tool]}))
    synthesis=tmp_path/'request_02.json'
    with_evidence='task\n\n## Evidence collected during the session\n\n[tool result]\nfn matched() {}\n\n'+CAUSAL_GUIDANCE
    synthesis.write_text(json.dumps({**base,'messages':[{'role':'system','content':'neutral'},
                                                        {'role':'user','content':with_evidence}]}))
    assert audit_wire(tmp_path,'task','neutral',config,digest([tool]))
    for content in ('task\n\n'+CAUSAL_GUIDANCE,
                    'task\n\n## Evidence collected during the session\n\n(no tool evidence was collected)\n\n'+CAUSAL_GUIDANCE,
                    with_evidence.replace(CAUSAL_GUIDANCE,'')):
        synthesis.write_text(json.dumps({**base,'messages':[{'role':'system','content':'neutral'},
                                                            {'role':'user','content':content}]}))
        assert not audit_wire(tmp_path,'task','neutral',config,digest([tool])),(content[:60],)


def test_accounting_inconsistency_is_technical_failure():
    row={'prompt_tokens':10,'completion_tokens':3,'total_tokens':13,'usage_per_request':[{'prompt_tokens':10,'completion_tokens':3,'total_tokens':13}]}
    assert accounting_valid(row)
    row['usage_per_request'][0]['total_tokens']=12
    assert not accounting_valid(row)
    row['usage_per_request'][0]['total_tokens']=None
    assert not accounting_valid(row)


def test_wire_audit_detects_prompt_tool_and_inference_contamination(tmp_path):
    config={'model':'m','temperature':0,'max_output_tokens':4096,'request_options':{},'max_iterations':25}
    payload={'model':'m','temperature':0,'max_tokens':4096,'stream':False,
             'messages':[{'role':'system','content':'neutral'},{'role':'user','content':'original task'}],
             'tools':[{'type':'function','function':{'name':'read_file'}}]}
    file=tmp_path/'request_01.json'
    schema=digest(payload['tools'])
    file.write_text(json.dumps(payload))
    assert audit_wire(tmp_path,'original task','neutral',config,schema)
    for mutate in ('skill','tool','reasoning','extra_system'):
        changed=copy.deepcopy(payload)
        if mutate=='skill':changed['messages'][0]['content']+=' SKILL LEAK'
        if mutate=='tool':changed['tools'].append({'type':'function','function':{'name':'mcp_tool'}})
        if mutate=='reasoning':changed['reasoning_effort']='medium'
        if mutate=='extra_system':changed['messages'].append({'role':'system','content':'INHERITED SKILL'})
        file.write_text(json.dumps(changed))
        assert not audit_wire(tmp_path,'original task','neutral',config,schema)


def test_pilot_stops_on_execution_failure_but_not_partial_quality():
    model={'id':'m'}
    manifest={'schedule':[['t','A',1]],'config':{'max_iterations':25,'max_output_tokens':4096,'context_window':131072},
              'technical_readiness':{'baseline_rows':[{'configuration':'A','tool_schema_sha256':'native'}]},
              'skill_sha256':'skill','task_prompt_sha256':{'t':'task'},'system_prompt_sha256':{'A':'neutral'},
              'preflight':{'model':model}}
    row={'task_id':'t','configuration':'A','run_number':1,'status':'completed','correctness_score':0.5,
         'response_metadata':[{'finish_reason':'stop'}],'provider_protocol_errors':0,'tool_events':[],
         'repository_unchanged':True,'inputs_unchanged':True,'skill_loaded':False,'loaded_skill_sha256':None,'mcp_available':False,'mcp_tool_calls':0,
         'tool_schema_sha256':'native','task_prompt_sha256':'task','system_prompt_sha256':'neutral',
         'model_metadata_before':{'object':'list','data':[model]},'model_metadata_after':{'object':'list','data':[model]},
         'usage_per_request':[{'prompt_tokens':10,'completion_tokens':3,'total_tokens':13}],
         'prompt_tokens':10,'completion_tokens':3,'total_tokens':13,'wire_controls_verified':True,'environment_unchanged':True}
    assert validate_pilot([row],manifest)['technical_passed']
    row['tool_events']=[{'error':'invalid_arguments'}]  # Correct rejection, followed by recovery.
    assert validate_pilot([row],manifest)['continue_execution']
    for change in ('transport','status','schema','isolation'):
        changed=copy.deepcopy(row)
        if change=='transport':changed['tool_events']=[{'error':'tool_transport_error'}]
        if change=='status':changed['status']='error'
        if change=='schema':changed['tool_schema_sha256']='changed'
        if change=='isolation':changed['skill_loaded']=True
        assert not validate_pilot([changed],manifest)['continue_execution']


def test_pilot_admission_allows_partial_smoke_and_reporting_changes(tmp_path):
    from bench.harness.experiments.pilot import CONTROL_FIELDS, EXECUTION_SOURCES
    old={'phase':'smoke','identity':'historical','skill_sha256':'skill',
         'config':{k:None for k in CONTROL_FIELDS}, 'preflight':{'model':{'meta':{'n_ctx':131072}}},
         'repo':{'commit':'same'}, 'prompt_set_sha256':'prompts','provider_endpoint_sha256':'endpoint',
         'embedding_configuration':{},'capture_traces':True,
         'mcp_binary_sha256':'binary','harness_sources':{'bench/harness/'+k:'unchanged' for k in EXECUTION_SOURCES}}
    old['config'].update(context_window=131072,max_iterations=25,max_output_tokens=4096)
    rows=[{'task_id':'task','configuration':a,'run_number':1,'status':'completed','tool_calls':1,
           'failed_tool_calls':0,'provider_protocol_errors':0,'tool_events':[{'error':None}],
           'mcp_tool_calls':int(a!='A'),'mcp_available':a!='A',
           'skill_loaded':a=='C','loaded_skill_sha256':'skill' if a=='C' else None,
           'task_prompt_sha256':'same','system_prompt_sha256':'skill' if a=='C' else 'base',
           'tool_schema_sha256':'native' if a=='A' else 'mcp','repository_unchanged':True,'inputs_unchanged':True,
           'prompt_tokens':10,'completion_tokens':3,'total_tokens':13,'task_success':False,'correctness_score':0.5,
           'response_metadata':[{'finish_reason':'stop'}],
           'usage_per_request':[{'prompt_tokens':10,'completion_tokens':3,'total_tokens':13}]} for a in 'ABC']
    (tmp_path/'run_manifest.json').write_text(json.dumps(old))
    (tmp_path/'results.jsonl').write_text('\n'.join(json.dumps(r) for r in rows))
    for a in 'ABC':
        wire=tmp_path/f'instances/task/{a}/run_1/wire';wire.mkdir(parents=True)
        (wire/'request_01.json').write_text(json.dumps({'tools':[a]}))
    current=copy.deepcopy(old);current['harness_sources']['bench/harness/experiments/report.py']='new'
    assert require_technical_smoke(tmp_path,current)['admission']=='technical_only'
    assert 'correctness_score' not in json.dumps(require_technical_smoke(tmp_path,current))
    current['mcp_binary_sha256']='changed'
    with pytest.raises(ValueError,match='mcp_binary_sha256'):
        require_technical_smoke(tmp_path,current)
    current.pop('mcp_binary_sha256')
    with pytest.raises(ValueError,match='mcp_binary_sha256'):
        require_technical_smoke(tmp_path,current)
    current['mcp_binary_sha256']='binary'
    current['config']['temperature']=1
    with pytest.raises(ValueError,match='temperature'):
        require_technical_smoke(tmp_path,current)
    current=copy.deepcopy(old)
    current['harness_sources']['bench/harness/framework/executors.py']='changed'
    with pytest.raises(ValueError,match='execution source'):
        require_technical_smoke(tmp_path,current)


def test_prompt_controller_is_part_of_pilot_admission():
    # The neutral system prompt and schedule are treatment, not reporting.
    assert 'experiments/navigation_skill.py' in EXECUTION_SOURCES
    assert 'experiments/report.py' not in EXECUTION_SOURCES
    assert 'experiments/task_quality.py' not in EXECUTION_SOURCES


def _pilot_case():
    model = {'id': 'm'}
    manifest = {'schedule': [['t', 'A', 1]],
                'config': {'max_iterations': 25, 'max_output_tokens': 4096, 'context_window': 131072},
                'technical_readiness': {'baseline_rows': [{'configuration': 'A', 'tool_schema_sha256': 'native'}]},
                'skill_sha256': 'skill', 'task_prompt_sha256': {'t': 'task'},
                'system_prompt_sha256': {'A': 'neutral'}, 'preflight': {'model': model}}
    row = {'task_id': 't', 'configuration': 'A', 'run_number': 1, 'status': 'completed',
           'correctness_score': None, 'response_metadata': [{'finish_reason': 'stop'}],
           'provider_protocol_errors': 0, 'tool_events': [], 'repository_unchanged': True,
           'inputs_unchanged': True, 'skill_loaded': False, 'loaded_skill_sha256': None,
           'mcp_available': False, 'mcp_tool_calls': 0, 'tool_schema_sha256': 'native',
           'task_prompt_sha256': 'task', 'system_prompt_sha256': 'neutral',
           'model_metadata_before': {'object': 'list', 'data': [model]},
           'model_metadata_after': {'object': 'list', 'data': [model]},
           'usage_per_request': [{'prompt_tokens': 10, 'completion_tokens': 3, 'total_tokens': 13}],
           'prompt_tokens': 10, 'completion_tokens': 3, 'total_tokens': 13,
           'wire_controls_verified': True, 'environment_unchanged': True}
    return manifest, row


def test_model_stability_tolerates_sibling_models_and_extra_keys():
    # Endpoints commonly list more than one model and add top-level keys.
    manifest, row = _pilot_case()
    row['model_metadata_before'] = {'object': 'list', 'has_more': False,
                                    'data': [{'id': 'other-model'}, {'id': 'm'}]}
    row['model_metadata_after'] = {'object': 'list', 'data': [{'id': 'm'}, {'id': 'other-model'}]}
    validation = validate_pilot([row], manifest)
    assert validation['checks']['model_unchanged'] and validation['technical_passed']
    # The selected entry must still be present and stable on both samples.
    changed = copy.deepcopy(row)
    changed['model_metadata_after'] = {'object': 'list', 'data': [{'id': 'other-model'}]}
    assert not validate_pilot([changed], manifest)['checks']['model_unchanged']
    changed = copy.deepcopy(row)
    changed['model_metadata_before'] = {'unavailable': 'provider_http_500'}
    assert not validate_pilot([changed], manifest)['checks']['model_unchanged']
    changed = copy.deepcopy(row)
    changed['model_metadata_after'] = {'object': 'list', 'data': [{'id': 'm', 'meta': {'n_ctx': 8192}}]}
    assert not validate_pilot([changed], manifest)['checks']['model_unchanged']


def _smoke_case():
    rows = [{'task_id': 't', 'configuration': arm, 'run_number': 1, 'status': 'completed',
             'correctness_score': None, 'task_success': None, 'tool_calls': 1, 'failed_tool_calls': 0,
             'provider_protocol_errors': 0, 'tool_events': [{'error': None}],
             'mcp_tool_calls': int(arm != 'A'), 'mcp_available': arm != 'A',
             'skill_loaded': arm == 'C', 'loaded_skill_sha256': 'skill' if arm == 'C' else None,
             'task_prompt_sha256': 'task', 'system_prompt_sha256': 'skill' if arm == 'C' else 'neutral',
             'tool_schema_sha256': 'native' if arm == 'A' else 'mcp',
             'repository_unchanged': True, 'inputs_unchanged': True,
             'response_metadata': [{'finish_reason': 'stop'}],
             'usage_per_request': [{'prompt_tokens': 10, 'completion_tokens': 3, 'total_tokens': 13}],
             'prompt_tokens': 10, 'completion_tokens': 3, 'total_tokens': 13} for arm in 'ABC']
    manifest = {'phase': 'smoke', 'skill_sha256': 'skill',
                'config': {'max_iterations': 25, 'context_window': 131072, 'max_output_tokens': 4096},
                'preflight': {'model': {'id': 'm', 'meta': {'n_ctx': 131072}}}}
    return rows, manifest


def test_smoke_gate_allows_recovered_tool_errors_but_not_infrastructure_ones():
    rows, manifest = _smoke_case()
    # A correctly rejected argument that the agent recovered from is an observation.
    rows[1]['tool_events'] = [{'error': 'invalid_arguments'}, {'error': None}]
    rows[1]['failed_tool_calls'] = 1
    validation = validate_smoke(rows, manifest)
    assert validation['checks']['tools_exercised_and_valid']
    assert validation['technical_passed']
    for fault in ('transport', 'protocol', 'none_succeeded'):
        changed = copy.deepcopy(rows)
        if fault == 'transport':
            changed[0]['tool_events'] = [{'error': 'tool_transport_error'}]
        if fault == 'protocol':
            changed[0]['provider_protocol_errors'] = 1
        if fault == 'none_succeeded':
            changed[0]['tool_events'] = [{'error': 'invalid_arguments'}]
        assert not validate_smoke(changed, manifest)['checks']['tools_exercised_and_valid']
