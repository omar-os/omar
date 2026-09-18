#!/usr/bin/env python3
"""Produce a credential-free, descriptive report of every selected trial."""
import argparse
import json
import hashlib
from pathlib import Path

SAFE_FIELDS = ['case_id','variant','revision','parent','children','scenario','seed','models',
    'human_reminders','budget_seconds','outcome','task_sha256','both_children_ready_s','released_s',
    'completed_s','elapsed_s','artifact_valid','worker_execution_attributed','orphan_sessions',
    'manager_reported_completion','result_receipts','terminal_unacknowledged','failure_phase',
    'result_to_retirement_seconds','unretired_result_seconds']

def retirement_metrics(events,elapsed):
    outputs={}; retired={}
    for event in events:
        now=event['seconds']; state=event['state']
        for role in ['left','right']:
            if role in state['outputs']: outputs.setdefault(role,now)
            if 'bench-0-'+role in state['sessions']: retired.pop(role,None)
            elif role in outputs: retired.setdefault(role,now)
    return {'result_to_retirement_seconds':{r:round(retired[r]-t,3) for r,t in outputs.items() if r in retired},
            'unretired_result_seconds':{r:round(elapsed-t,3) for r,t in outputs.items() if r not in retired}}


def main():
    p=argparse.ArgumentParser()
    p.add_argument('runs',nargs='+',type=Path)
    p.add_argument('--json',type=Path,required=True)
    p.add_argument('--markdown',type=Path,required=True)
    p.add_argument('--assessments',type=Path,help='Reviewed setup/diagnostic annotations keyed by batch/case_id')
    a=p.parse_args(); results=[]; manifests=[]
    assessments=json.loads(a.assessments.read_text()) if a.assessments else {}
    for root in a.runs:
        manifest=json.loads((root/'manifest.json').read_text()); manifests.append({'batch':root.name,**manifest})
        for variant,parent,scenario,seed in sorted(manifest['cases']):
            case_id=f'{variant}-{parent}-{scenario}-{seed}'
            case=root/case_id/'result.json'
            if case.exists(): raw=json.loads(case.read_text())
            else:
                assignment=case.parent/'assignment.txt'
                raw={'case_id':case_id,'variant':variant,'parent':parent,'scenario':scenario,'seed':seed,
                    'revision':manifest['revisions'][variant],'outcome':'missing_result','error':True,
                    'task_sha256':hashlib.sha256(assignment.read_bytes()).hexdigest() if assignment.exists() else None}
            value={k:raw[k] for k in SAFE_FIELDS if k in raw}
            value['batch']=root.name
            value['assessment']=assessments.get(root.name+'/'+value['case_id'],{})
            # Recompute timings from preserved state traces: an earlier dead
            # incarnation is not retirement of a later result-producing worker.
            events=json.loads((case.parent/'events.json').read_text()) if (case.parent/'events.json').exists() else []
            if raw.get('elapsed_s') is not None:
                value.update(retirement_metrics(events,raw['elapsed_s']))
            if raw.get('outcome')=='timeout' and events:
                value['failure_phase']='result_collection_or_retirement' if len(events[-1]['state']['outputs'])==2 else 'delegated_work' if raw.get('both_children_ready_s') is not None else 'startup_or_delegation'
            if 'intervention' in raw:
                value['intervention']={k:raw['intervention'][k] for k in ['kind','verified','seconds'] if k in raw['intervention']}
            # Error details may contain native provider responses; keep raw logs
            # private and publish only whether the case had a harness error.
            value['harness_error']=bool(raw.get('error'))
            results.append(value)
    keyed={}
    for result in results:
        key=(result['batch'],result['parent'],result['scenario'],result['seed'])
        assert result['variant'] not in keyed.setdefault(key,{}), 'duplicate paired trial'
        keyed[key][result['variant']]=result
    lines=['# Legacy workflow benchmark results','',
        'Live model trials against PR #254. No human reminders or terminal submission.',
        'These are descriptive results for a small fixed suite, not a general success-rate estimate.', '',
        '| Batch | Parent | Scenario | Seed | Baseline | Candidate | Baseline seconds | Candidate seconds |',
        '| --- | --- | --- | --- | --- | --- | --- | --- |']
    counts={'both_passed':0,'candidate_only':0,'baseline_only':0,'neither_passed':0,'invalid_or_unpaired':0}
    for (batch,parent,scenario,seed),pair in sorted(keyed.items()):
        left,right=pair.get('baseline',{}),pair.get('candidate',{})
        if left and right:
            assert left['task_sha256']==right['task_sha256'], 'paired tasks differ'
        def label(value):
            if not value:return 'not run'
            status=value['outcome']
            if value.get('failure_phase'):status+=': '+value['failure_phase']
            if value.get('assessment',{}).get('exclude_comparison'):status+=' (excluded)'
            if value.get('intervention') and not value['intervention'].get('verified'):status+=' (intervention unverified)'
            return status
        lines.append(f"| {batch} | {parent} | {scenario} | {seed} | {label(left)} | {label(right)} | {left.get('elapsed_s','—')} | {right.get('elapsed_s','—')} |")
        if not left or not right or any(v.get('harness_error') or v.get('assessment',{}).get('exclude_comparison') for v in [left,right]):
            counts['invalid_or_unpaired']+=1
        else:
            lp,rp=left['outcome']=='passed',right['outcome']=='passed'
            counts['both_passed' if lp and rp else 'candidate_only' if rp else 'baseline_only' if lp else 'neither_passed']+=1
    lines+=['','Paired outcomes: '+', '.join(f'{k}={v}' for k,v in counts.items())+'.','',
        'Timeouts are deadline-censored and are not successful completion times. An unverified',
        'context intervention cannot establish compaction/recovery behavior. Startup/delivery',
        'failures must be distinguished from failures after delegated work completed.','',
        'A pass requires correct aggregation, worker-attributed execution, zero surviving child',
        'sessions and a completion report from the model-driven PM. Integrated results also',
        'record unacknowledged terminal tasks. The scripted EA does not manage the children.','',
        'Model IDs, source revisions, binary hashes, harness hash, budgets and all trial outcomes',
        'are included in the adjacent JSON. Native account state and raw transcripts are excluded.','']
    a.json.parent.mkdir(parents=True,exist_ok=True)
    a.json.write_text(json.dumps({'manifests':manifests,'paired_outcomes':counts,'trials':results},indent=2,sort_keys=True)+'\n')
    a.markdown.write_text('\n'.join(lines))

if __name__=='__main__':main()
