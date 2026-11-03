#!/usr/bin/env python3
"""Deterministic diagnostics. Semantic acceptance still needs blinded human review."""
import ast, json, subprocess, sys, operator
from pathlib import Path

BUILTINS = {"sum","min","max","len","sorted","set","list","abs","range","enumerate"}
ALLOWED = (ast.Module,ast.FunctionDef,ast.arguments,ast.arg,ast.Return,ast.If,ast.Assign,ast.AnnAssign,ast.Expr,ast.Name,ast.Load,ast.Store,ast.Constant,ast.List,ast.Tuple,ast.Set,ast.Dict,ast.ListComp,ast.GeneratorExp,ast.comprehension,ast.Subscript,ast.Slice,ast.BinOp,ast.UnaryOp,ast.BoolOp,ast.Compare,ast.IfExp,ast.Call,ast.Add,ast.Sub,ast.Mult,ast.Div,ast.FloorDiv,ast.Mod,ast.USub,ast.UAdd,ast.Not,ast.And,ast.Or,ast.Eq,ast.NotEq,ast.Lt,ast.LtE,ast.Gt,ast.GtE,ast.In,ast.NotIn)

def validate_code(source):
    if len(source)>20000: raise ValueError("Source exceeds diagnostic budget")
    tree=ast.parse(source)
    if len(tree.body)!=1 or not isinstance(tree.body[0],ast.FunctionDef) or tree.body[0].name!="repair": raise ValueError("Expected only repair(values)")
    function=tree.body[0]
    if function.decorator_list or function.args.defaults or len(function.args.args)!=1 or function.args.args[0].arg!="values": raise ValueError("Unexpected function signature")
    for node in ast.walk(tree):
        if not isinstance(node,ALLOWED): raise ValueError(f"Forbidden syntax: {type(node).__name__}")
        if isinstance(node,ast.Name) and node.id.startswith("__"): raise ValueError("Dunder names forbidden")
        if isinstance(node,ast.Constant) and isinstance(node.value,(int,float)) and abs(node.value)>1000000: raise ValueError("Numeric constant exceeds diagnostic budget")
        if isinstance(node,ast.Call) and (not isinstance(node.func,ast.Name) or node.func.id not in BUILTINS or node.keywords): raise ValueError("Unapproved function call")
    return tree

def run_code(source, tests):
    validate_code(source)
    result=subprocess.run([sys.executable,"-I",str(Path(__file__).resolve()),"--child"],input=json.dumps({"source":source,"tests":tests}),text=True,capture_output=True,timeout=3)
    if result.returncode: raise ValueError("Protected code checks failed: "+result.stderr[-300:])
    return json.loads(result.stdout)

class Returned(Exception):
    def __init__(self,value):self.value=value

class Interpreter:
    """No Python exec/eval or host objects exposed to candidate code."""
    def __init__(self):self.remaining=20000
    def tick(self):
        self.remaining-=1
        if self.remaining<0:raise ValueError("Operation budget exceeded")
    def bounded(self,value):
        if isinstance(value,(list,tuple,set,dict,str)) and len(value)>10000:raise ValueError("Collection budget exceeded")
        if isinstance(value,(int,float)) and abs(value)>1e12:raise ValueError("Numeric budget exceeded")
        return value
    def expression(self,node,env):
        self.tick()
        if isinstance(node,ast.Constant):return node.value
        if isinstance(node,ast.Name):return env[node.id]
        if isinstance(node,(ast.List,ast.Tuple,ast.Set)):return self.bounded([self.expression(n,env) for n in node.elts])
        if isinstance(node,ast.IfExp):return self.expression(node.body if self.expression(node.test,env) else node.orelse,env)
        if isinstance(node,ast.Slice):return slice(*(self.expression(v,env) if v else None for v in [node.lower,node.upper,node.step]))
        if isinstance(node,ast.Subscript):return self.expression(node.value,env)[self.expression(node.slice,env)]
        if isinstance(node,ast.UnaryOp):return {ast.USub:operator.neg,ast.UAdd:operator.pos,ast.Not:operator.not_}[type(node.op)](self.expression(node.operand,env))
        if isinstance(node,ast.BinOp):
            left,right=self.expression(node.left,env),self.expression(node.right,env)
            if isinstance(node.op,ast.Mult) and (isinstance(left,(str,list,tuple)) or isinstance(right,(str,list,tuple))):
                seq,n=(left,right) if isinstance(right,int) else (right,left)
                if not isinstance(n,int) or len(seq)*max(n,0)>10000:raise ValueError("Repetition budget exceeded")
            return self.bounded({ast.Add:operator.add,ast.Sub:operator.sub,ast.Mult:operator.mul,ast.Div:operator.truediv,ast.FloorDiv:operator.floordiv,ast.Mod:operator.mod}[type(node.op)](left,right))
        if isinstance(node,ast.Compare):
            left=self.expression(node.left,env)
            for op,right_node in zip(node.ops,node.comparators):
                right=self.expression(right_node,env)
                fn={ast.Eq:operator.eq,ast.NotEq:operator.ne,ast.Lt:operator.lt,ast.LtE:operator.le,ast.Gt:operator.gt,ast.GtE:operator.ge,ast.In:lambda a,b:a in b,ast.NotIn:lambda a,b:a not in b}[type(op)]
                if not fn(left,right):return False
                left=right
            return True
        if isinstance(node,ast.BoolOp):
            for part in node.values:
                result=self.expression(part,env)
                if (isinstance(node.op,ast.And) and not result) or (isinstance(node.op,ast.Or) and result):return result
            return result
        if isinstance(node,(ast.ListComp,ast.GeneratorExp)):
            values=[]
            def collect(index,scope):
                self.tick()
                if index==len(node.generators):values.append(self.expression(node.elt,scope));self.bounded(values);return
                gen=node.generators[index]
                if not isinstance(gen.target,ast.Name):raise ValueError("Only simple comprehension bindings are supported")
                for value in self.expression(gen.iter,scope):
                    child={**scope,gen.target.id:value}
                    if all(self.expression(test,child) for test in gen.ifs):collect(index+1,child)
            collect(0,env);return values
        if isinstance(node,ast.Call):
            args=[self.expression(v,env) for v in node.args];name=node.func.id
            if name=="range":
                value=range(*args)
                if len(value)>10000:raise ValueError("Range budget exceeded")
                return list(value)
            if name=="enumerate":return list(enumerate(*args))
            import builtins
            return self.bounded(getattr(builtins,name)(*args))
        raise ValueError(f"Unsupported diagnostic expression: {type(node).__name__}")
    def statements(self,body,env):
        for node in body:
            self.tick()
            if isinstance(node,ast.Return):raise Returned(self.expression(node.value,env) if node.value else None)
            if isinstance(node,ast.If):self.statements(node.body if self.expression(node.test,env) else node.orelse,env)
            elif isinstance(node,(ast.Assign,ast.AnnAssign)):
                targets=node.targets if isinstance(node,ast.Assign) else [node.target]
                if any(not isinstance(t,ast.Name) for t in targets):raise ValueError("Input mutation is forbidden")
                value=self.expression(node.value,env)
                for target in targets:env[target.id]=value
            elif isinstance(node,ast.Expr):self.expression(node.value,env)
            elif not isinstance(node,ast.If):raise ValueError("Unsupported diagnostic statement")
    def run(self,tree,values):
        try:self.statements(tree.body[0].body,{"values":values})
        except Returned as result:return result.value
        return None

def grade(task, reference, output, *, repository_image=None, repository_artifacts=None):
    if task["family"]=="repository_repair":
        if not repository_image or not repository_artifacts:
            return {"deterministic_pass":None,"errors":["repository evaluation requires an explicitly pinned local image and artifact directory"],"evaluation_status":"infrastructure_failure","semantic_acceptance":None,"human_review_status":"pending","label_review_status":reference["review_status"],"human_minutes":None,"substantive_edits":None}
        from repository_grade import grade_repository
        return grade_repository(task,reference,output,image=repository_image,artifacts=repository_artifacts)
    errors=[]
    try:
        if isinstance(output,str): output=json.loads(output)
        if task["family"]=="document_synthesis":
            if output.get("decision")!=reference["decision"]: errors.append("wrong_decision")
            claims=output.get("claims",[]);ids={s["id"] for s in task["sources"]}
            if not claims: errors.append("missing_claims")
            cited=set()
            for claim in claims:
                citations=claim.get("source_ids",[]);cited.update(citations)
                if not citations or not set(citations)<=ids: errors.append("invalid_citation")
                if not claim.get("text"): errors.append("empty_claim")
            if not set(reference["required_source_ids"])<=cited: errors.append("incomplete_evidence_coverage")
            if not output.get("limitations"): errors.append("missing_limitations")
        else:
            checks=run_code(output["patched_source"],reference["protected_tests"])
            errors.extend(checks["errors"])
    except Exception as error: errors.append(f"invalid_output: {error}")
    return {"deterministic_pass":not errors,"errors":errors,"semantic_acceptance":None,"human_review_status":"pending","label_review_status":reference["review_status"],"human_minutes":None,"substantive_edits":None}

if __name__=="__main__" and "--child" in sys.argv:
    import resource
    resource.setrlimit(resource.RLIMIT_CPU,(1,1))
    data=json.load(sys.stdin); tree=validate_code(data["source"])
    errors=[]
    for values,expected in data["tests"]:
        original=list(values)
        if Interpreter().run(tree,values)!=expected: errors.append("protected_regression_failure")
        if values!=original: errors.append("input_mutation")
    print(json.dumps({"errors":errors}))
