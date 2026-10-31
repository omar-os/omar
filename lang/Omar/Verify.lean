import Lean
import Omar.Reactor

/-!
# Accepting a proof

`#omar_verify name` is the last line of every property file `omarc --prove`
writes. A property counts as proved only if this command accepts it:

* its statement has the form `∀ e : Execution, Valid topology e → …`, about
  the model `omarc` generated, so it speaks of every run of this program;
* the kernel checked it using no axioms beyond Lean's standard three, which
  is what rules out `sorry` and anything else assumed rather than proved.

A statement must also say something about the run: it depends on `e`, and
names at least one of the program's ports, reactions or timers, all of which
exist. `True`, or a property of a port nobody spelled right, is vacuous.

It reports the names the statement mentions, which are what the property is
about, so the operator can be shown those components.
-/

namespace Omar.Verify

open Lean Elab Command

/-- The axioms any Lean development may use; anything else is an assumption. -/
def standardAxioms : List Name := [``propext, ``Classical.choice, ``Quot.sound]

/-- What `type` says of a run, if it is `∀ e : Execution, Valid topology e → _`:
    the rest, in which `e` is bound variable 1. -/
def aboutEveryRun (type : Expr) : Option Expr :=
  match type with
  | .forallE _ (.const ``Omar.Reactor.Execution []) (.forallE _ valid body _) _ =>
    match valid with
    | .app (.app (.const ``Omar.Reactor.Valid []) (.const `topology [])) (.bvar 0) => some body
    | _ => none
  | _ => none

/-- The string literals in an expression: the ports, reactions and timers a
    statement names. -/
partial def literals (e : Expr) (acc : Array String := #[]) : Array String :=
  match e with
  | .lit (.strVal s) => if acc.contains s then acc else acc.push s
  | .app f a => literals a (literals f acc)
  | .lam _ t b _ | .forallE _ t b _ => literals b (literals t acc)
  | .letE _ t v b _ => literals b (literals v (literals t acc))
  | .mdata _ b | .proj _ _ b => literals b acc
  | _ => acc

elab "#omar_verify " id:ident : command => do
  let name ← liftCoreM <| realizeGlobalConstNoOverloadWithInfo id
  let info ← getConstInfo name
  let axioms ← collectAxioms name
  let extra := axioms.filter (!standardAxioms.contains ·)
  let model ← getConstInfo `topology
  let names := literals model.value!
  let mentions := literals info.type
  let unknown := mentions.filter (!names.contains ·)
  let problem :=
    if !(info matches .thmInfo _) then some "not a theorem"
    else match aboutEveryRun info.type with
    | none => some "the statement must be about every run: `∀ e : Execution, Valid topology e → …`"
    | some body =>
      if !body.hasLooseBVar 1 then some "the statement says nothing about the run `e`"
      else if mentions.isEmpty then some "the statement names no port, reaction or timer of the program"
      else if !unknown.isEmpty then some s!"the program has no {unknown.toList}"
      else if !extra.isEmpty then some s!"it relies on {extra.toList} rather than proving it"
      else none
  match problem with
  | some problem => logError m!"property {name} is not accepted: {problem}"
  | none =>
    let verdict := Json.mkObj [
      ("property", toJson name.toString),
      ("axioms", toJson (axioms.map (·.toString))),
      ("mentions", toJson mentions)]
    logInfo m!"omar-verdict {verdict.compress}"

end Omar.Verify
