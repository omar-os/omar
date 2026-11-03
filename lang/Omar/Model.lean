import Omar.Compiler
import Omar.Reactor

/-!
# A program's model, for proofs

The topology a proof is about is the one this compiler elaborated, converted
here and printed as Lean source (`omarc --lean`). Only the structure the
semantics speaks of is kept: prompts and code bodies are left out, since a
proof may not depend on what they say.
-/

namespace Omar.Model

open Omar.Reactor

/-- A contract constant as the runtime compares it: `true`, `false` and
    numbers as written, anything else a JSON string with its quotes trimmed
    (`parse_literal` in `topology.rs`). -/
def constantJson (token : String) : String :=
  if token == "true" || token == "false" || (!token.isEmpty && token.all Char.isDigit) then token
  else
    let inner := ((token.dropWhile (· == '"')).dropEndWhile (· == '"')).toString
    (Lean.Json.str inner).compress

/-- One group of a contract, as `parse_contract_group` reads it. -/
def clause (tokens : List String) : Clause :=
  let optional := tokens.getLast? == some "?"
  let tokens := if optional then tokens.dropLast else tokens
  let tokens :=
    if tokens.head? == some "(" && tokens.getLast? == some ")" then (tokens.drop 1).dropLast
    else tokens
  let atoms := tokens.foldr
    (fun token (atoms : List (List String)) =>
      if token == "|" then [] :: atoms
      else match atoms with
        | atom :: rest => (token :: atom) :: rest
        | [] => [[token]])
    [[]]
  { optional
    alternatives := atoms.filterMap fun
      | name :: "=" :: value :: _ => some { port := name, constant := some (constantJson value) }
      | name :: _ => some { port := name }
      | [] => none }

/-- A contract as `parse_contract` reads it: groups split at top-level commas. -/
def contract (text : String) : List Clause :=
  let tokens := (text.splitOn " ").filter (!·.isEmpty)
  let (groups, current, _) := tokens.foldl
    (fun (groups, current, depth) token =>
      match token with
      | "(" => (groups, current ++ [token], depth + 1)
      | ")" => (groups, current ++ [token], depth - 1)
      | "," => if depth == 0 then (groups ++ [current], [], depth)
               else (groups, current ++ [token], depth)
      | _ => (groups, current ++ [token], depth))
    (([] : List (List String)), ([] : List String), (0 : Nat))
  let groups := if current.isEmpty then groups else groups ++ [current]
  groups.map clause

def ofProgram (program : Omar.Program) : Topology where
  name := program.team
  instances := program.instances.toList.map fun i =>
    { name := i.name, team := i.team, parent := i.parent }
  agents := program.agents.toList.map fun a =>
    { name := a.name, backend := a.backend, owner := a.instance_ }
  ports := program.ports.toList.map fun p =>
    { name := p.name, owner := p.instance_, delay := p.delay
      kind := match p.kind with
        | .input => .input
        | .output => .output
        | .action => .action }
  timers := program.timers.toList.map fun t =>
    { name := t.name, offset := t.offset, period := t.period, shutdown := t.shutdown
      owner := t.instance_ }
  connections := program.connections.toList.map fun c =>
    { source := c.source, target := c.target, delay := c.delay }
  reactions := program.reactions.toList.map fun r =>
    { id := r.id, owner := r.instance_, agent := r.agent, triggers := r.triggers.toList
      contract := contract r.contract }

/-! ## Printing the model as Lean source -/

private def str (s : String) : String := s.quote

private def optNat : Option Nat → String
  | none => "none"
  | some n => s!"some {n}"

private def list (items : List String) : String :=
  match items with
  | [] => "[]"
  | _ => "[\n    " ++ String.intercalate ",\n    " items ++ "]"

private def kind : Kind → String
  | .input => ".input"
  | .output => ".output"
  | .action => ".action"

private def effect (e : Effect) : String :=
  match e.constant with
  | none => s!"\{ port := {str e.port} }"
  | some c => s!"\{ port := {str e.port}, constant := some {str c} }"

private def clauseText (c : Clause) : String :=
  s!"\{ alternatives := [{String.intercalate ", " (c.alternatives.map effect)}], \
    optional := {c.optional} }"

/-- The model as a Lean definition named `topology`. -/
def render (t : Topology) : String :=
  let instances := t.instances.map fun i =>
    s!"\{ name := {str i.name}, team := {str i.team}, parent := {str i.parent} }"
  let agents := t.agents.map fun a =>
    s!"\{ name := {str a.name}, backend := {str a.backend}, owner := {str a.owner} }"
  let ports := t.ports.map fun p =>
    s!"\{ name := {str p.name}, kind := {kind p.kind}, delay := {optNat p.delay}, \
      owner := {str p.owner} }"
  let timers := t.timers.map fun x =>
    s!"\{ name := {str x.name}, offset := {x.offset}, period := {x.period}, \
      shutdown := {x.shutdown}, owner := {str x.owner} }"
  let connections := t.connections.map fun c =>
    s!"\{ source := {str c.source}, target := {str c.target}, delay := {optNat c.delay} }"
  let reactions := t.reactions.map fun r =>
    s!"\{ id := {str r.id}, owner := {str r.owner}, agent := {str r.agent},\n      \
      triggers := [{String.intercalate ", " (r.triggers.map str)}],\n      \
      contract := [{String.intercalate ", " (r.contract.map clauseText)}] }"
  s!"/-- `{t.name}` as `omarc` compiled it: the structure the semantics speaks of,
    without prompts or code bodies. -/
def topology : Omar.Reactor.Topology where
  name := {str t.name}
  instances := {list instances}
  agents := {list agents}
  ports := {list ports}
  timers := {list timers}
  connections := {list connections}
  reactions := {list reactions}
"

end Omar.Model
