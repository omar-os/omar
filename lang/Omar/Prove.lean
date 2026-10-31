import Lean.Data.Json
import Omar.Model

/-!
# Checking properties (`omarc --prove`)

A property arrives as a name, a statement and a proof. It is checked in a
file of its own, so one property's mistake never hides another's verdict:

```
import Omar.Reactor
import Omar.Verify
open Omar.Reactor

<the model omarc generated for this program>

theorem name (e : Execution) (hv : Valid topology e) :
    <statement> := <proof>

#omar_verify name
```

The model and the frame around the statement are written here, never by
whoever wrote the proof, and `#omar_verify` has the last word on whether the
result counts. Text that could change what Lean accepts rather than prove
something is refused before Lean sees it.
-/

namespace Omar.Prove

open Lean

structure Property where
  name : String
  statement : String
  proof : String
  deriving FromJson

/-- What `--prove` is given: the properties, and lemmas they share. A lemma
    is checked like everything else, and one that is not proved spoils every
    property that uses it, since `#omar_verify` follows the axioms through. -/
structure Request where
  lemmas : String := ""
  properties : Array Property

/-- `lemmas` may be left out; a derived instance would insist on it. -/
instance : FromJson Request where
  fromJson? json := do
    let properties ← json.getObjValAs? (Array Property) "properties"
    pure { properties, lemmas := (json.getObjValAs? String "lemmas").toOption.getD "" }

/-- Words that change what Lean checks or assumes rather than prove anything. -/
def refused : List String :=
  ["sorry", "admit", "axiom", "native_decide", "implemented_by", "extern", "unsafe",
   "opaque", "set_option", "#exit", "import", "macro", "elab", "syntax", "notation",
   "attribute", "run_cmd", "run_tac", "run_elab", "ofReduceBool", "skipKernelTC", "Lean."]

def validName (name : String) : Bool :=
  match name.toList with
  | c :: rest => (c.isAlpha || c == '_') && rest.all (fun c => c.isAlphanum || c == '_')
  | [] => false

/-- Why a property is refused unread, if it is. -/
def screen (lemmas : String) (p : Property) : Option String :=
  if !validName p.name then
    some s!"'{p.name}' is not a name: use letters, digits and underscores"
  else match refused.find? (fun word => (lemmas ++ "\n" ++ p.statement ++ "\n" ++ p.proof).contains word) with
    | some word => some s!"'{word}' is not allowed in a property or lemma; prove it instead"
    | none => none

/-- Everything before the theorem: the model, then the shared lemmas. -/
def header (model lemmas : String) : String :=
  "import Omar.Reactor\nimport Omar.Verify\nopen Omar.Reactor\n\n" ++ model ++ "\n" ++
    (if lemmas.trimAscii.isEmpty then "" else lemmas.trimAscii.toString ++ "\n\n")

/-- The theorem as it is checked and shown. -/
def theoremText (p : Property) : String :=
  s!"theorem {p.name} (e : Execution) (hv : Valid topology e) :\n    {p.statement.trimAscii} := \
    {p.proof.trimAscii}\n"

def file (model lemmas : String) (p : Property) : String :=
  header model lemmas ++ theoremText p ++ s!"\n#omar_verify {p.name}\n"

structure Problem where
  line : Nat
  column : Nat
  message : String
  deriving ToJson

structure Verdict where
  name : String
  proved : Bool
  /-- The theorem, as checked; problem lines count from its first line. -/
  «theorem» : String
  /-- The whole file, for opening in an editor. -/
  source : String
  mentions : Array String := #[]
  problems : Array Problem := #[]
  deriving ToJson

def position (json : Json) (field : String) : Nat :=
  ((json.getObjVal? "pos").toOption.bind (·.getObjValAs? Nat field |>.toOption)).getD 0

/-- The verdict read off Lean's messages for one file (`--json`: an object a
    line). The theorem starts on the line after the header. -/
def verdictOf (model lemmas : String) (p : Property) (output : String) : Verdict := Id.run do
  let before := ((header model lemmas).splitOn "\n").length - 1
  let mut problems := #[]
  let mut mentions := #[]
  let mut accepted := false
  for line in output.splitOn "\n" do
    let .ok json := Json.parse line | continue
    let severity := (json.getObjValAs? String "severity").toOption.getD ""
    let data := (json.getObjValAs? String "data").toOption.getD ""
    if severity == "information" && data.startsWith "omar-verdict " then
      if let .ok verdict := Json.parse (data.drop "omar-verdict ".length).toString then
        mentions := (verdict.getObjValAs? (Array String) "mentions").toOption.getD #[]
        accepted := true
    else if severity == "error" then
      let row := position json "line"
      problems := problems.push
        { line := if row > before then row - before else 0, column := position json "column"
          message := data }
  return { name := p.name, proved := accepted && problems.isEmpty, «theorem» := theoremText p,
           source := file model lemmas p, mentions, problems }

/-- How to run Lean: `OMAR_LEAN` if set, else `lean`, asking elan for the
    toolchain this compiler was built with unless `lean` already is it. The
    library is found beside `omarc` in Lake's layout (`bin/../lib/lean`)
    unless `OMAR_LEAN_PATH` says where. -/
def leanCommand : IO (String × Array String × String) := do
  let library ← match ← IO.getEnv "OMAR_LEAN_PATH" with
    | some path => pure path
    | none => pure ((← IO.appDir) / ".." / "lib" / "lean").toString
  if let some lean ← IO.getEnv "OMAR_LEAN" then return (lean, #[], library)
  let version ← try
      pure (← IO.Process.output { cmd := "lean", args := #["--short-version"] }).stdout
    catch _ => pure ""
  if version.trimAscii.toString == Lean.versionString then return ("lean", #[], library)
  pure ("lean", #[s!"+leanprover/lean4:v{Lean.versionString}"], library)

def check (model : String) (request : Request) (scratch : System.FilePath) :
    IO (Array Verdict) := do
  IO.FS.createDirAll scratch
  let (lean, args, library) ← leanCommand
  let lemmas := request.lemmas
  let running ← request.properties.mapM fun p => do
    match screen lemmas p with
    | some problem =>
      pure (Sum.inl { name := p.name, proved := false, «theorem» := theoremText p,
                      source := file model lemmas p,
                      problems := #[{ line := 0, column := 0, message := problem }] : Verdict })
    | none =>
      let path := scratch / s!"{p.name}.lean"
      IO.FS.writeFile path (file model lemmas p)
      -- Lean runs once a property, all at once.
      let task ← IO.asTask (IO.Process.output {
        cmd := lean, args := args ++ #["--json", path.toString]
        env := #[("LEAN_PATH", some library)] })
      pure (Sum.inr (p, task))
  running.mapM fun
    | .inl verdict => pure verdict
    | .inr (p, task) => do
      let output ← IO.ofExcept task.get
      let verdict := verdictOf model lemmas p output.stdout
      if !verdict.proved && verdict.problems.isEmpty then
        let why := output.stderr.trimAscii.toString
        let message := if why.isEmpty then "Lean did not accept the property" else why
        pure { verdict with problems := #[{ line := 0, column := 0, message }] }
      else pure verdict

end Omar.Prove
