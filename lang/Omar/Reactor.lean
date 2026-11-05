/-!
# The reactor semantics OMAR programs are verified against

A property of a program is a theorem about every execution the runtime could
produce from it. This file says what such an execution is: not by stepping a
machine, but by axioms every run satisfies (an axiomatic model, after Lin et
al., "Towards Building Verifiable CPS using Lingua Franca", TECS 2023).

What a proof may rely on is only what the runtime guarantees whatever an
agent does:

* the topology: ports, connections and their delays, timers, reactions and
  the ports each may and must write (`spec.md` §2–§4);
* effect contracts, which the runtime checks before an invocation completes;
* tags: a value lands at the tag its hop costs, and nothing happens after a
  run ends.

What a prompt asks for, what an agent answers and what a code body computes
are not known: a reaction may write any value its contract allows. A proof
that holds here therefore holds for every model, prompt and backend.

Axioms that say something *must* happen (a trigger invokes its reaction, a
write arrives downstream) hold only before a run is cut short, because a
failure or an operator's stop abandons the tag it happens at. Axioms that say
something happens *only* for a reason hold always.

`valid_exists` shows the axioms are consistent, so no property can hold
merely because no execution satisfies them.
-/

namespace Omar.Reactor

/-- A logical time: nanoseconds since the run began, and a microstep. -/
structure Tag where
  time : Nat
  micro : Nat
  deriving DecidableEq, Repr

namespace Tag

def zero : Tag := ⟨0, 0⟩

/-- Lexicographic: time first, then microstep. -/
protected def lt (a b : Tag) : Prop :=
  a.time < b.time ∨ (a.time = b.time ∧ a.micro < b.micro)

instance : LT Tag := ⟨Tag.lt⟩
instance : LE Tag := ⟨fun a b => a < b ∨ a = b⟩

instance (a b : Tag) : Decidable (a < b) :=
  inferInstanceAs (Decidable (a.time < b.time ∨ (a.time = b.time ∧ a.micro < b.micro)))

instance (a b : Tag) : Decidable (a ≤ b) :=
  inferInstanceAs (Decidable (a < b ∨ a = b))

/-- Where a hop costing `delay` lands a value written at `g`: a plain hop at
    `g`, `after 0` one microstep later, and `after d` `d` nanoseconds later at
    microstep zero (`spec.md` §4). -/
def after (g : Tag) : Option Nat → Tag
  | none => g
  | some 0 => ⟨g.time, g.micro + 1⟩
  | some (d + 1) => ⟨g.time + (d + 1), 0⟩

end Tag

inductive Kind where
  | input | output | action
  deriving DecidableEq, Repr

structure Port where
  name : String
  kind : Kind
  /-- An action's fixed delay. `none` costs a microstep. -/
  delay : Option Nat := none
  owner : String
  deriving DecidableEq, Repr

structure Connection where
  source : String
  target : String
  delay : Option Nat := none
  deriving DecidableEq, Repr

/-- `timer t(offset, period)`, or the `shutdown` trigger a team reacts to. -/
structure Timer where
  name : String
  offset : Nat := 0
  period : Nat := 0
  shutdown : Bool := false
  owner : String
  deriving DecidableEq, Repr

/-- One effect a contract names, and the value it must carry if it is a
    constant (`accepted = false`), rendered as JSON. -/
structure Effect where
  port : String
  constant : Option String := none
  deriving DecidableEq, Repr

/-- One comma-separated group of a contract: `a`, `a?`, `(a | b)`, `(a | b)?`.
    A required group writes exactly one alternative, an optional one at most
    one. -/
structure Clause where
  alternatives : List Effect
  optional : Bool := false
  deriving DecidableEq, Repr

structure Reaction where
  id : String
  owner : String
  /-- The agent that answers it, or empty for a code reaction. -/
  agent : String := ""
  triggers : List String
  contract : List Clause
  deriving DecidableEq, Repr

structure Instance where
  name : String
  team : String
  parent : String := ""
  deriving DecidableEq, Repr

structure Agent where
  name : String
  backend : String
  owner : String
  deriving DecidableEq, Repr

/-- A program as the runtime runs it: every name qualified by its instance,
    reactions in declaration order. -/
structure Topology where
  name : String
  instances : List Instance
  agents : List Agent
  ports : List Port
  timers : List Timer
  connections : List Connection
  reactions : List Reaction
  deriving Repr

/-- A value as JSON text. What it is never matters to a proof except where a
    contract fixes it. -/
abbrev Value := String

namespace Topology

variable (T : Topology)

def port? (name : String) : Option Port := T.ports.find? (·.name == name)

def reaction? (id : String) : Option Reaction := T.reactions.find? (·.id == id)

/-- The ports a reaction may write: every port its contract names. -/
def Reaction.mayWrite (r : Reaction) : List String :=
  r.contract.flatMap fun clause => clause.alternatives.map (·.port)

/-- The reactions that may write `port`. -/
def writers (port : String) : List String :=
  (T.reactions.filter fun r => (Reaction.mayWrite r).contains port).map (·.id)

/-- The connections into `port`. -/
def sources (port : String) : List Connection :=
  T.connections.filter (·.target == port)

/-- Where a value written to `port` at `g` is present: an input or output at
    `g`; an action after its delay, a microstep if it has none. -/
def land (port : String) (g : Tag) : Tag :=
  match T.port? port with
  | some { kind := .action, delay, .. } => g.after (some (delay.getD 0))
  | _ => g

/-- The position of a reaction in declaration order. -/
def index (id : String) : Nat := T.reactions.findIdx (·.id == id)

/-- Ports a value written to `port` reaches at the same tag: through plain
    connections, which cost nothing. Bounded by the number of connections,
    since a chain longer than that repeats one. -/
def sameTag (port : String) : List String :=
  go T.connections.length [port] [port]
where
  go : Nat → List String → List String → List String
    | 0, _, seen => seen
    | _ + 1, [], seen => seen
    | fuel + 1, p :: rest, seen =>
      let next := (T.connections.filter fun c => c.source == p && c.delay.isNone).map (·.target)
      let fresh := next.filter (!seen.contains ·)
      go fuel (rest ++ fresh) (seen ++ fresh)

/-- `a` must run before `b` at a tag both run at (`spec.md` §4.0): `a` writes
    what reaches a trigger of `b` at no cost, or they share a written port,
    an agent or an instance and `a` is declared first. -/
def precedes (a b : Reaction) : Bool :=
  (Reaction.mayWrite a).any (fun p => (T.sameTag p).any (b.triggers.contains ·)) ||
  (T.index a.id < T.index b.id &&
    ((Reaction.mayWrite a).any ((Reaction.mayWrite b).contains ·) ||
     (!a.agent.isEmpty && a.agent == b.agent) ||
     a.owner == b.owner))

end Topology

/-- How a run ends, if it does. -/
inductive Ending where
  /-- Still running, or never stops: every tag can be reached. -/
  | never
  /-- Ended cleanly: `shutdown` fires at `last`, and that tag completes. -/
  | shutdown (last : Tag)
  /-- Cut short at `last` by a failure or an operator's stop: that tag may be
      left part done, and `shutdown` does not fire. -/
  | cut (last : Tag)
  deriving DecidableEq, Repr

/-- One run: what every port held at every tag, and what every reaction did.
    Nothing here is computed; `Valid` says which of these are runs. -/
structure Execution where
  /-- The value port `p` holds at tag `g` once the tag is done, or `none`
      when it is absent. -/
  present : Tag → String → Option Value
  /-- Reaction `r` was invoked at `g`. -/
  invoked : Tag → String → Prop
  /-- Its invocation at `g` completed, the runtime having accepted its writes. -/
  completed : Tag → String → Prop
  /-- What it wrote to port `p` at `g`. -/
  wrote : Tag → String → String → Option Value
  /-- When it started, relative to the other invocations at `g`. -/
  order : Tag → String → Nat
  ending : Ending

namespace Execution

variable (e : Execution)

/-- The run reaches `g`: it has not ended before it. -/
def reaches (g : Tag) : Prop :=
  match e.ending with
  | .never => True
  | .shutdown last | .cut last => g ≤ last

/-- `g` is a tag the run completes: reached, and not one it was cut short at. -/
def completes (g : Tag) : Prop :=
  match e.ending with
  | .never => True
  | .shutdown last => g ≤ last
  | .cut last => g < last

end Execution

section Semantics

variable (T : Topology) (e : Execution)

/-- A timer fires at `g`: at its offset, then every period after; once if the
    period is zero. Shutdown fires at the tag a clean run ends at. -/
def timerFires (t : Timer) (g : Tag) : Prop :=
  if t.shutdown then e.ending = .shutdown g
  else g.micro = 0 ∧ t.offset ≤ g.time ∧
    (if t.period = 0 then g.time = t.offset else (g.time - t.offset) % t.period = 0)

/-- Trigger `name` is present at `g`: a port holding a value, or a timer
    firing. -/
def triggered (g : Tag) (name : String) : Prop :=
  (e.present g name).isSome ∨ ∃ t ∈ T.timers, t.name = name ∧ timerFires e t g

/-- The writes of one completed invocation honour its contract: each group
    gets exactly one alternative (at most one if optional), constants carry
    their value, and nothing outside the contract is written. -/
def honours (r : Reaction) (writes : String → Option Value) : Prop :=
  (∀ p, (writes p).isSome → p ∈ Topology.Reaction.mayWrite r) ∧
  ∀ clause ∈ r.contract,
    (if clause.optional then
      (clause.alternatives.filter fun a => (writes a.port).isSome).length ≤ 1
    else
      (clause.alternatives.filter fun a => (writes a.port).isSome).length = 1) ∧
    ∀ a ∈ clause.alternatives, ∀ c, a.constant = some c → ∀ v, writes a.port = some v → v = c

/-- The axioms of a run of `T`. -/
structure Valid : Prop where
  /-- Only declared reactions are invoked, only while the run lasts, and only
      when one of their triggers is present. -/
  invoked_cause : ∀ g id, e.invoked g id →
    e.reaches g ∧ ∃ r ∈ T.reactions, r.id = id ∧ ∃ t ∈ r.triggers, triggered T e g t
  /-- A present trigger invokes its reaction, at any tag the run completes. -/
  invoked_of_trigger : ∀ g, ∀ r ∈ T.reactions, ∀ t ∈ r.triggers,
    e.completes g → triggered T e g t → e.invoked g r.id
  /-- Completion follows invocation; an invocation that never completes cuts
      the run at its tag. -/
  completed_invoked : ∀ g id, e.completed g id → e.invoked g id
  completed_or_cut : ∀ g id, e.invoked g id → e.completed g id ∨ e.ending = .cut g
  /-- Only completed invocations have written anything, and what they wrote
      honours their contract. -/
  wrote_completed : ∀ g id p, (e.wrote g id p).isSome → e.completed g id
  honoured : ∀ g, ∀ r ∈ T.reactions, e.completed g r.id → honours r (e.wrote g r.id)
  /-- A value is present only for a reason: a reaction wrote it (landing after
      the port's delay), a connection carried it (after the connection's
      delay), or the operator supplied it to an input when the run began. -/
  present_cause : ∀ h p v, e.present h p = some v → e.reaches h ∧
    ((∃ g id, e.wrote g id p = some v ∧ T.land p g = h) ∨
     (∃ c ∈ T.connections, c.target = p ∧ ∃ g, g.after c.delay = h ∧ e.present g c.source = some v) ∨
     (∃ port ∈ T.ports, port.name = p ∧ port.kind = .input ∧ h = Tag.zero))
  /-- Writes and connections deliver, at any tag the run completes. -/
  wrote_lands : ∀ g id p, (e.wrote g id p).isSome → e.completes (T.land p g) →
    (e.present (T.land p g) p).isSome
  connection_carries : ∀ c ∈ T.connections, ∀ g, (e.present g c.source).isSome →
    e.completes (g.after c.delay) → (e.present (g.after c.delay) c.target).isSome
  /-- At a tag, a reaction that must precede another starts first. -/
  ordered : ∀ g, ∀ a ∈ T.reactions, ∀ b ∈ T.reactions, T.precedes a b →
    e.invoked g a.id → e.invoked g b.id → e.order g a.id < e.order g b.id

/-- The axioms are consistent: every topology has a run, the one cut short at
    its first tag before anything happens. So a property is never proved by
    there being no runs to check. -/
theorem valid_exists : ∃ e, Valid T e := by
  refine ⟨{ present := fun _ _ => none, invoked := fun _ _ => False,
            completed := fun _ _ => False, wrote := fun _ _ _ => none,
            order := fun _ _ => 0, ending := .cut Tag.zero }, ?_⟩
  exact
    { invoked_cause := fun _ _ h => h.elim
      invoked_of_trigger := fun g _ _ _ _ hc _ => by
        have hlt : Tag.lt g Tag.zero := hc
        rcases hlt with h | ⟨_, h⟩ <;> exact absurd h (Nat.not_lt_zero _)
      completed_invoked := fun _ _ h => h
      completed_or_cut := fun _ _ h => h.elim
      wrote_completed := fun _ _ _ h => by simp at h
      honoured := fun _ _ _ h => h.elim
      present_cause := fun _ _ _ h => by simp at h
      wrote_lands := fun _ _ _ h => by simp at h
      connection_carries := fun _ _ _ h _ => by simp at h
      ordered := fun _ _ _ _ _ _ h => h.elim }

end Semantics

/-! ## Lemmas properties are proved with

Each turns an axiom about some port or reaction into a finite case split the
topology decides: `simp [topology, Topology.writers, …]` evaluates the lists. -/

section Lemmas

variable {T : Topology} {e : Execution}

/-- A present value was written by one of the port's writers, carried by one
    of its connections, or supplied to an input at the start. -/
theorem present_from (hv : Valid T e) {h p v} (hp : e.present h p = some v) :
    (∃ g id, id ∈ T.writers p ∧ e.completed g id ∧ e.wrote g id p = some v ∧ T.land p g = h) ∨
    (∃ c ∈ T.sources p, ∃ g, g.after c.delay = h ∧ e.present g c.source = some v) ∨
    (∃ port ∈ T.ports, port.name = p ∧ port.kind = .input ∧ h = Tag.zero) := by
  rcases (hv.present_cause h p v hp).2 with ⟨g, id, hw, hl⟩ | ⟨c, hc, ht, g, hg, hs⟩ | hi
  · have hdone := hv.wrote_completed g id p (by simp [hw])
    obtain ⟨r, hr, rfl, -⟩ := (hv.invoked_cause g id (hv.completed_invoked g id hdone)).2
    have hmay := (hv.honoured g r hr hdone).1 p (by simp [hw])
    refine Or.inl ⟨g, r.id, ?_, hdone, hw, hl⟩
    simp only [Topology.writers, List.mem_map, List.mem_filter, List.contains_iff_mem]
    exact ⟨r, ⟨hr, by simpa using hmay⟩, rfl⟩
  · exact Or.inr (Or.inl ⟨c, by simp [Topology.sources, hc, ht], g, hg, hs⟩)
  · exact Or.inr (Or.inr hi)

/-- An invoked reaction had a trigger present. -/
theorem invoked_from (hv : Valid T e) {g id} (hi : e.invoked g id) :
    ∃ r ∈ T.reactions, r.id = id ∧ ∃ t ∈ r.triggers, triggered T e g t :=
  (hv.invoked_cause g id hi).2

/-- A completed reaction's writes honour its contract. -/
theorem completed_honours (hv : Valid T e) {g} {r : Reaction} (hr : r ∈ T.reactions)
    (hc : e.completed g r.id) : honours r (e.wrote g r.id) :=
  hv.honoured g r hr hc

end Lemmas

end Omar.Reactor
