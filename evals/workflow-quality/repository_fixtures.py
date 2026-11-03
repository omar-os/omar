"""Owned multi-file Python repositories. Related mutations stay in one split.

These are diagnostic repositories, not samples of arbitrary production projects.
Labels/oracle patches are authored here and still need independent review.
"""
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def operation(name, spec, correct, broken, cases):
    return dict(name=name, spec=spec, correct=correct, broken=broken, cases=cases)


REPOSITORIES = [
    ("schedule", "development", [
        operation("merge", "Merge sorted or unsorted integer [start,end] intervals, including touching intervals; preserve the input.",
                  "out=[]\nfor start,end in sorted(p):\n    if out and start <= out[-1][1]: out[-1][1]=max(out[-1][1],end)\n    else: out.append([start,end])\nreturn out",
                  "return sorted(p)", [([[4,6],[1,4]],[[1,6]]), ([],[]), ([[1,9],[2,3],[12,14]],[[1,9],[12,14]])]),
        operation("overlap", "Return the duration shared by two half-open integer intervals, never negative.",
                  "return max(0,min(p[0][1],p[1][1])-max(p[0][0],p[1][0]))",
                  "return min(p[0][1],p[1][1])-max(p[0][0],p[1][0])", [([[0,2],[4,6]],0), ([[0,5],[2,4]],2), ([[0,2],[2,4]],0)]),
        operation("due", "Return task IDs whose ISO dates are on or before today, retaining input order.",
                  "today=date.fromisoformat(p['today'])\nreturn [t['id'] for t in p['tasks'] if date.fromisoformat(t['due']) <= today]",
                  "return [t['id'] for t in p['tasks'] if t['due'] < p['today']]", [({'today':'2024-02-29','tasks':[{'id':'a','due':'2024-02-29'}]},['a']), ({'today':'2024-03-01','tasks':[{'id':'b','due':'2024-02-29'},{'id':'c','due':'2024-03-02'}]},['b']), ({'today':'2024-01-01','tasks':[]},[])]),
        operation("slots", "Return all whole [start,end] slots of positive length within a day range; include the last exact-fit slot.",
                  "return [[s,s+p['length']] for s in range(p['start'],p['end']-p['length']+1,p['length'])]",
                  "return [[s,s+p['length']] for s in range(p['start'],p['end']-p['length'],p['length'])]", [({'start':0,'end':6,'length':3},[[0,3],[3,6]]), ({'start':5,'end':6,'length':3},[]), ({'start':3,'end':9,'length':2},[[3,5],[5,7],[7,9]])]),
        operation("elapsed", "Return elapsed whole minutes between timezone-aware ISO timestamps, accounting for offsets.",
                  "return int((datetime.fromisoformat(p['end'])-datetime.fromisoformat(p['start'])).total_seconds()//60)",
                  "return int((datetime.fromisoformat(p['end']).replace(tzinfo=None)-datetime.fromisoformat(p['start']).replace(tzinfo=None)).total_seconds()//60)", [({'start':'2024-01-01T10:00:00+02:00','end':'2024-01-01T09:30:00+00:00'},90), ({'start':'2024-01-01T23:30:00+00:00','end':'2024-01-02T00:00:00+00:00'},30), ({'start':'2024-01-01T10:00:00+00:00','end':'2024-01-01T10:00:00+00:00'},0)]),
    ]),
    ("inventory", "development", [
        operation("reserve", "Reserve every requested quantity atomically. If any stock is insufficient return accepted=false and the unchanged stock. Never mutate input.",
                  "stock=dict(p['stock'])\nif any(stock.get(k,0)<v for k,v in p['request'].items()): return {'accepted':False,'stock':stock}\nfor k,v in p['request'].items(): stock[k]=stock.get(k,0)-v\nreturn {'accepted':True,'stock':stock}",
                  "stock=dict(p['stock'])\nfor k,v in p['request'].items(): stock[k]=stock.get(k,0)-v\nreturn {'accepted':True,'stock':stock}", [({'stock':{'a':2},'request':{'a':3}},{'accepted':False,'stock':{'a':2}}), ({'stock':{'a':2,'b':1},'request':{'a':1,'b':2}},{'accepted':False,'stock':{'a':2,'b':1}}), ({'stock':{'a':2},'request':{'a':2}},{'accepted':True,'stock':{'a':0}})]),
        operation("replay", "Sum event quantity deltas, applying only the first occurrence of each event ID.",
                  "seen=set(); total=0\nfor e in p:\n    if e['id'] not in seen: total+=e['delta']; seen.add(e['id'])\nreturn total",
                  "return sum(e['delta'] for e in p)", [([{'id':'x','delta':3},{'id':'x','delta':3}],3), ([],0), ([{'id':'a','delta':4},{'id':'b','delta':-2},{'id':'a','delta':9}],2)]),
        operation("reorder", "Return sorted SKUs whose stock is at or below the reorder point; use zero for missing stock.",
                  "return sorted(k for k,v in p['points'].items() if p['stock'].get(k,0)<=v)",
                  "return sorted(k for k,v in p['points'].items() if p['stock'].get(k,0)<v)", [({'points':{'a':2},'stock':{'a':2}},['a']), ({'points':{'b':0,'a':1},'stock':{}},['a','b']), ({'points':{'a':2},'stock':{'a':3}},[])]),
        operation("allocate", "Allocate requested quantity FIFO from positive batch counts. Return allocations and unmet quantity; do not mutate batches.",
                  "need=p['quantity']; allocations=[]\nfor batch in p['batches']:\n    take=min(need,batch['count'])\n    if take: allocations.append({'id':batch['id'],'count':take})\n    need-=take\nreturn {'allocations':allocations,'unmet':need}",
                  "need=p['quantity']; allocations=[]\nfor batch in reversed(p['batches']):\n    take=min(need,batch['count'])\n    if take: allocations.append({'id':batch['id'],'count':take})\n    need-=take\nreturn {'allocations':allocations,'unmet':need}", [({'quantity':3,'batches':[{'id':'a','count':2},{'id':'b','count':4}]},{'allocations':[{'id':'a','count':2},{'id':'b','count':1}],'unmet':0}), ({'quantity':3,'batches':[]},{'allocations':[],'unmet':3}), ({'quantity':0,'batches':[{'id':'a','count':2}]},{'allocations':[],'unmet':0})]),
        operation("available", "Available stock is max(on_hand-reserved,0), per SKU; preserve all on-hand keys.",
                  "return {k:max(v-p['reserved'].get(k,0),0) for k,v in p['on_hand'].items()}",
                  "return {k:v-p['reserved'].get(k,0) for k,v in p['on_hand'].items()}", [({'on_hand':{'a':2},'reserved':{'a':4}},{'a':0}), ({'on_hand':{'a':2,'b':3},'reserved':{'a':1}},{'a':1,'b':3}), ({'on_hand':{},'reserved':{}},{})]),
    ]),
    ("records", "development", [
        operation("versions", "Sort dot-separated numeric version strings by numeric components, preserving duplicates.",
                  "return sorted(p,key=lambda v:tuple(int(n) for n in v.split('.')))",
                  "return sorted(p)", [(['1.10','1.2'],['1.2','1.10']), ([],[]), (['2.0','1.9','1.9'],['1.9','1.9','2.0'])]),
        operation("csv_row", "Encode one CSV row using standard CSV quoting and no trailing line terminator.",
                  "out=io.StringIO(); csv.writer(out,lineterminator='\\n').writerow(p)\nreturn out.getvalue()[:-1]",
                  "return ','.join(p)", [(['a,b','c'],'"a,b",c'), (['say "yes"','x'],'"say ""yes""",x'), (['a','b'],'a,b')]),
        operation("merge", "Merge override fields into defaults. An explicit null overrides a default; absent fields preserve it. Never mutate inputs.",
                  "return {**p['defaults'],**p['overrides']}",
                  "return {**p['defaults'],**{k:v for k,v in p['overrides'].items() if v is not None}}", [({'defaults':{'x':1},'overrides':{'x':None}},{'x':None}), ({'defaults':{'x':1},'overrides':{}},{'x':1}), ({'defaults':{},'overrides':{'y':2}},{'y':2})]),
        operation("page", "Return a one-based page of records of positive size; an out-of-range page returns empty.",
                  "start=(p['page']-1)*p['size']\nreturn p['items'][start:start+p['size']]",
                  "start=p['page']*p['size']\nreturn p['items'][start:start+p['size']]", [({'page':1,'size':2,'items':[1,2,3]},[1,2]), ({'page':2,'size':2,'items':[1,2,3]},[3]), ({'page':4,'size':2,'items':[1]},[])]),
        operation("latest", "Keep the record with greatest numeric revision for each ID; on ties keep the first. Return records sorted by ID.",
                  "best={}\nfor r in p:\n    if r['id'] not in best or r['revision']>best[r['id']]['revision']: best[r['id']]=r\nreturn [best[k] for k in sorted(best)]",
                  "return list({r['id']:r for r in p}.values())", [([{'id':'a','revision':3},{'id':'a','revision':1}],[{'id':'a','revision':3}]), ([],[]), ([{'id':'b','revision':1},{'id':'a','revision':1}],[{'id':'a','revision':1},{'id':'b','revision':1}])]),
    ]),
    ("delivery", "development", [
        operation("parcels", "Split a nonnegative integer item count into full capacity parcels then one remainder; omit zero-size parcels.",
                  "full,rest=divmod(p['count'],p['capacity'])\nreturn [p['capacity']]*full+([rest] if rest else [])",
                  "full,rest=divmod(p['count'],p['capacity'])\nreturn [p['capacity']]*full+[rest]", [({'count':6,'capacity':3},[3,3]), ({'count':0,'capacity':4},[]), ({'count':7,'capacity':3},[3,3,1])]),
        operation("zones", "Choose the longest matching postal prefix; return its zone or null when no prefix matches.",
                  "matches=[k for k in p['zones'] if p['postal'].startswith(k)]\nreturn p['zones'][max(matches,key=len)] if matches else None",
                  "return next((v for k,v in p['zones'].items() if p['postal'].startswith(k)),None)", [({'postal':'12345','zones':{'1':'broad','123':'local'}},'local'), ({'postal':'999','zones':{'1':'x'}},None), ({'postal':'123','zones':{'12':'x'}},'x')]),
        operation("billable", "Billable kilograms are ceiling(max(actual grams, dimensional grams)/1000), with zero weight billed zero.",
                  "return (max(p['actual'],p['dimensional'])+999)//1000",
                  "return max(p['actual'],p['dimensional'])//1000", [({'actual':1001,'dimensional':0},2), ({'actual':0,'dimensional':0},0), ({'actual':500,'dimensional':2000},2)]),
        operation("events", "Return unique tracking event IDs in first-seen order.",
                  "return list(dict.fromkeys(p))",
                  "return sorted(set(p))", [(['b','a','b'],['b','a']), ([],[]), (['x','x'],['x'])]),
        operation("charge", "Compute decimal base plus per-kg rate times weight, rounding half up to exactly two decimal places as text.",
                  "amount=Decimal(p['base'])+Decimal(p['rate'])*p['kg']\nreturn str(amount.quantize(Decimal('0.01'),rounding=ROUND_HALF_UP))",
                  "return str(Decimal(p['base'])+Decimal(p['rate'])*p['kg'])", [({'base':'1','rate':'0.335','kg':1},'1.34'), ({'base':'0','rate':'0','kg':0},'0.00'), ({'base':'2.00','rate':'1.20','kg':2},'4.40')]),
    ]),
    ("permissions", "reserved", [
        operation("decision", "Deny overrides allow. Return true only if requested action is allowed and not denied.",
                  "return p['action'] in p['allow'] and p['action'] not in p['deny']",
                  "return p['action'] in p['allow']", [({'action':'read','allow':['read'],'deny':['read']},False), ({'action':'read','allow':['read'],'deny':[]},True), ({'action':'write','allow':['read'],'deny':[]},False)]),
        operation("within", "Normalize POSIX absolute paths, then check whether path equals root or is its descendant, using path components.",
                  "root=posixpath.normpath(p['root']); path=posixpath.normpath(p['path'])\nreturn path==root or path.startswith(root.rstrip('/')+'/')",
                  "return posixpath.normpath(p['path']).startswith(posixpath.normpath(p['root']))", [({'root':'/team/a','path':'/team/ab'},False), ({'root':'/team/a','path':'/team/a/x'},True), ({'root':'/team/a','path':'/team/a/../b'},False)]),
        operation("roles", "Union permissions from all assigned known roles and return sorted unique strings; ignore unknown roles.",
                  "return sorted({x for role in p['assigned'] for x in p['definitions'].get(role,[])})",
                  "return sorted(p['definitions'].get(p['assigned'][0],[])) if p['assigned'] else []", [({'assigned':['a','b'],'definitions':{'a':['read'],'b':['write']}},['read','write']), ({'assigned':[],'definitions':{}},[]), ({'assigned':['unknown','a'],'definitions':{'a':['read']}},['read'])]),
        operation("expiry", "A grant is active when now is strictly before expires_at and revoked is false.",
                  "return not p['revoked'] and p['now']<p['expires_at']",
                  "return not p['revoked'] and p['now']<=p['expires_at']", [({'revoked':False,'now':10,'expires_at':10},False), ({'revoked':False,'now':9,'expires_at':10},True), ({'revoked':True,'now':9,'expires_at':10},False)]),
        operation("filter", "Keep records whose tenant exactly matches the requested tenant; tenant names are case-sensitive.",
                  "return [r for r in p['records'] if r['tenant']==p['tenant']]",
                  "return [r for r in p['records'] if r['tenant'].lower()==p['tenant'].lower()]", [({'tenant':'A','records':[{'tenant':'a','id':1}]},[]), ({'tenant':'A','records':[{'tenant':'A','id':2}]},[{'tenant':'A','id':2}]), ({'tenant':'A','records':[]},[])]),
    ]),
    ("subscriptions", "reserved", [
        operation("total", "Sum decimal item amounts and round half up to exactly two decimal places as text; empty total is 0.00.",
                  "return str(sum((Decimal(v) for v in p),Decimal('0')).quantize(Decimal('0.01'),rounding=ROUND_HALF_UP))",
                  "return str(sum((Decimal(v) for v in p),Decimal('0')))", [(['1.005'],'1.01'), ([], '0.00'), (['2.00','3.50'],'5.50')]),
        operation("discount", "Subtract discount cents, bounded below by zero; negative discounts are treated as zero.",
                  "return max(0,p['price']-max(0,p['discount']))",
                  "return p['price']-p['discount']", [({'price':100,'discount':150},0), ({'price':100,'discount':-10},100), ({'price':100,'discount':20},80)]),
        operation("periods", "Count whole periods due at or before now, starting at start, for positive integer interval; future starts return zero.",
                  "return max(0,(p['now']-p['start'])//p['interval']+1)",
                  "return max(0,(p['now']-p['start'])//p['interval'])", [({'start':0,'now':0,'interval':10},1), ({'start':10,'now':0,'interval':10},0), ({'start':0,'now':20,'interval':10},3)]),
        operation("balance", "Sum payments less refunds, deduplicating by event ID and keeping the first occurrence.",
                  "seen=set(); total=0\nfor e in p:\n    if e['id'] not in seen: total+=e['amount'] if e['kind']=='payment' else -e['amount']; seen.add(e['id'])\nreturn total",
                  "return sum(e['amount'] if e['kind']=='payment' else -e['amount'] for e in p)", [([{'id':'a','kind':'payment','amount':10},{'id':'a','kind':'payment','amount':10}],10), ([],0), ([{'id':'a','kind':'payment','amount':10},{'id':'b','kind':'refund','amount':3}],7)]),
        operation("active", "Return sorted IDs of subscriptions with start<=now<end; preserve separate records sharing an ID.",
                  "return sorted(s['id'] for s in p['subscriptions'] if s['start']<=p['now']<s['end'])",
                  "return sorted(s['id'] for s in p['subscriptions'] if s['start']<=p['now']<=s['end'])", [({'now':10,'subscriptions':[{'id':'a','start':0,'end':10}]},[]), ({'now':9,'subscriptions':[{'id':'a','start':0,'end':10}]},['a']), ({'now':0,'subscriptions':[]},[])]),
    ]),
]

IMPORTS = "import csv\nimport io\nimport posixpath\nfrom datetime import date, datetime\nfrom decimal import Decimal, ROUND_HALF_UP\n\n"


def source(operations, broken_index=None):
    return IMPORTS + "\n\n".join("def " + op["name"] + "(p):\n" + "\n".join("    " + line for line in op["broken" if i == broken_index else "correct"].splitlines()) for i, op in enumerate(operations)) + "\n"


def generate(destination=ROOT / "repository-repair"):
    for name, split, operations in REPOSITORIES:
        correct = {"candidate_app/policy.py":source(operations[:3]), "candidate_app/core.py":source(operations[3:])}
        api = "from . import policy, core\n\nOPERATIONS = {\n" + "".join(f"    {op['name']!r}: {'policy' if i<3 else 'core'}.{op['name']},\n" for i,op in enumerate(operations)) + "}\n\ndef handle(request):\n    return OPERATIONS[request['operation']](request['payload'])\n"
        protected = [{"id":f"{op['name']}-{i+1}","request":{"operation":op['name'],"payload":value},"expected":expected} for op in operations for i,(value,expected) in enumerate(op["cases"])]
        for variant, op in enumerate(operations):
            case=f"repo-{name}-{variant+1:02}"
            target="candidate_app/policy.py" if variant<3 else "candidate_app/core.py"
            files={**correct,"candidate_app/__init__.py":"from .api import handle\n","candidate_app/api.py":api,"pyproject.toml":f"[project]\nname = 'owned-{name}-fixture'\nversion = '0.1.0'\nrequires-python = '>=3.11'\n","README.md":"# Owned diagnostic repository: "+name+"\n\nPublic API: candidate_app.handle({operation, payload}). Inputs must remain unchanged.\n\n"+"\n".join(f"- {o['name']}: {o['spec']}" for o in operations)+"\n\nRun public checks with: python3 -m unittest discover -s tests\n"}
            files[target]=source(operations[:3] if variant<3 else operations[3:],variant if variant<3 else variant-3)
            public=next(t for t in protected if t['id']==op['name']+'-1')
            files["tests/test_public.py"]="import unittest\nfrom candidate_app import handle\n\nclass PublicReproduction(unittest.TestCase):\n    def test_reported_issue(self):\n        self.assertEqual(handle("+repr(public["request"])+"), "+repr(public["expected"])+")\n"
            task={"schema_version":1,"id":case,"family":"repository_repair","split":split,"group":"repository-"+name,"instruction":f"Repair the {name} package so its public API meets every README requirement. Reported defect: {op['spec']} Preserve unrelated behavior and inputs. Return JSON with changes [{'{'}path, content{'}'}] containing complete UTF-8 text for each modified allowed source file, and explanation. Do not modify tests, metadata or other paths.","files":files,"allowed_patch_paths":sorted([*correct,"candidate_app/api.py"]),"entrypoint":"candidate_app:handle","visible_test":public,"repository_digest":hashlib.sha256(json.dumps(files,sort_keys=True).encode()).hexdigest()}
            reference={"id":case,"protected_tests":protected,"oracle_patch":{"changes":[{"path":target,"content":correct[target]}],"explanation":"Authored fixture repair; independent review pending."},"label_author":"owned repository fixture generator","review_status":"provisional","critical_errors":["protected regression failure","input mutation","prohibited patch path","unexpected output or execution failure"],"human_review_required":["patch meets complete specification beyond examples","patch is reasonable and does not hard-code protected cases"],"independent_unit":"repository-"+name}
            for folder,value in [("inputs",task),("references",reference)]:
                path=destination/folder/(case+".json");path.parent.mkdir(parents=True,exist_ok=True);path.write_text(json.dumps(value,indent=2)+"\n")


if __name__ == "__main__":
    generate()
