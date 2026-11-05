/-!
# Mutation plans, and why they are right

A running program changed into another is described by a plan: the
declarations to remove, then those to declare or update. This file is the
whole of how a plan is computed, and proves it correct:

* `plan_sound`: executing the plan on the old program's declarations yields
  exactly the new program's;
* `plan_minimal`: a declaration the same in both, and not one the caller
  chose to replace, is never touched;
* `emit_sound`: the bytecode emitted for a plan, run by the bytecode's own
  semantics (`vm`), does what the plan does.

Both theorems hold whatever `forced` is: which declarations are replaced
outright (an instance with new arguments, and everything in it) is policy, and
no policy can make a plan wrong.

What is trusted rather than proved: reading the running program's bytecode
and printing the plan as JSON (`Compiler.lean`), and the Rust VM implementing
`vm`, which it also checks at run time before applying anything.
-/

namespace Omar.Mutation

/-- What a declaration declares. Two declarations with the same kind and
    qualified name are the same component in two programs. -/
inductive Kind where
  | type | inst | agent | port | timer | state | param | connection | reaction
  deriving DecidableEq, Repr

abbrev Key := Kind × String

/-- One declaration: its key, and everything else about it in a canonical
    form, so equal bodies mean the same declaration. -/
structure Decl where
  key : Key
  body : String
  deriving DecidableEq, Repr

abbrev Program := List Decl

/-- What a program declares, by key. -/
abbrev Store := Key → Option Decl

def load (p : Program) : Store := fun k => p.find? (fun d => decide (d.key = k))

def set (s : Store) (k : Key) (v : Option Decl) : Store :=
  fun k' => if k' = k then v else s k'

inductive Op where
  | remove (k : Key)
  | declare (d : Decl)
  | update (d : Decl)
  deriving DecidableEq, Repr

def Op.key : Op → Key
  | .remove k => k
  | .declare d => d.key
  | .update d => d.key

/-- An instruction is refused when it does not fit what is there: removing or
    updating something absent, declaring something present. -/
def step (s : Store) : Op → Option Store
  | .remove k => if (s k).isSome then some (set s k none) else none
  | .declare d => if (s d.key).isNone then some (set s d.key (some d)) else none
  | .update d => if (s d.key).isSome then some (set s d.key (some d)) else none

def exec (s : Store) : List Op → Option Store
  | [] => some s
  | op :: ops => (step s op).bind (exec · ops)

section Plan

variable (forced : Key → Bool) (old new : Program)

/-- Whether the old declaration under `k` goes: the new program has none, the
    caller replaces it, or it changed and is not a reaction (which is updated
    in place instead). -/
def removed (k : Key) : Bool :=
  match load old k, load new k with
  | none, _ => false
  | some _, none => true
  | some p, some n => forced k || (p != n && k.1 != .reaction)

/-- What the plan does for one declaration of the new program. -/
def addition (n : Decl) : Option Op :=
  match load old n.key with
  | none => some (.declare n)
  | some p =>
    if forced n.key then some (.declare n)
    else if p = n then none
    else if n.key.1 = .reaction then some (.update n)
    else some (.declare n)

/-- Removals first, in the reverse of declaration order; then what is new or
    changed, in the new program's order. -/
def plan : List Op :=
  ((old.reverse.filter (fun d => removed forced old new d.key)).map (fun d => .remove d.key)) ++
    new.filterMap (addition forced old)

end Plan

/-! ## Lemmas about `load` -/

def keys (p : Program) : List Key := p.map (·.key)

theorem load_eq_some {p : Program} {k : Key} {d : Decl} :
    load p k = some d → d ∈ p ∧ d.key = k := by
  intro h
  unfold load at h
  have := List.find?_some h
  exact ⟨List.mem_of_find?_eq_some h, by simpa using this⟩

theorem load_eq_none {p : Program} {k : Key} :
    load p k = none ↔ ∀ d ∈ p, d.key ≠ k := by
  unfold load
  simp [List.find?_eq_none]

theorem load_of_mem {p : Program} (nodup : (keys p).Nodup) {d : Decl} (mem : d ∈ p) :
    load p d.key = some d := by
  induction p with
  | nil => cases mem
  | cons x xs ih =>
    simp only [keys, List.map_cons, List.nodup_cons] at nodup
    unfold load
    rw [List.find?_cons]
    cases mem with
    | head => simp
    | tail _ mem =>
      have hne : x.key ≠ d.key := by
        intro heq
        exact nodup.1 (heq ▸ List.mem_map_of_mem mem)
      simp only [hne, decide_false]
      exact ih nodup.2 mem

theorem load_isSome_iff {p : Program} {k : Key} :
    (load p k).isSome ↔ k ∈ keys p := by
  constructor
  · intro h
    obtain ⟨d, hd⟩ := Option.isSome_iff_exists.mp h
    obtain ⟨mem, key⟩ := load_eq_some hd
    exact key ▸ List.mem_map_of_mem mem
  · intro h
    obtain ⟨d, mem, key⟩ := List.mem_map.mp h
    cases hl : load p k with
    | none => exact absurd key ((load_eq_none.mp hl) d mem)
    | some _ => rfl

/-! ## Executing removals, then declarations -/

/-- Removing distinct keys that are all present empties exactly those keys. -/
theorem exec_removals (s : Store) (ks : List Key) (nodup : ks.Nodup)
    (present : ∀ k ∈ ks, (s k).isSome) :
    exec s (ks.map .remove) = some (fun k => if k ∈ ks then none else s k) := by
  induction ks generalizing s with
  | nil => simp [exec]
  | cons k ks ih =>
    simp only [List.nodup_cons] at nodup
    simp only [List.map_cons, exec, step, present k (List.mem_cons_self ..), if_true,
      Option.bind_some]
    rw [ih (set s k none) nodup.2]
    · congr 1
      funext k'
      by_cases hk : k' = k
      · subst hk; simp [set]
      · simp [set, hk]
    · intro k' mem
      have : k' ≠ k := fun h => nodup.1 (h ▸ mem)
      simp [set, this, present k' (List.mem_cons_of_mem _ mem)]

/-- What an instruction leaves under its key. -/
def Op.result : Op → Option Decl
  | .remove _ => none
  | .declare d => some d
  | .update d => some d

/-- Whether a declaration or update finds what it needs. -/
def Fits (s : Store) : Op → Prop
  | .remove _ => False
  | .declare d => (s d.key).isNone
  | .update d => (s d.key).isSome

theorem fits_set {s : Store} {k : Key} {v : Option Decl} {o : Op} (ne : o.key ≠ k) :
    Fits (set s k v) o ↔ Fits s o := by
  cases o <;> simp_all [Fits, set, Op.key]

theorem step_fits {s : Store} {op : Op} (h : Fits s op) :
    step s op = some (set s op.key op.result) := by
  cases op <;> simp_all [Fits, step, Op.key, Op.result]

/-- Declarations and updates on distinct keys, each finding what it needs,
    leave each key holding its declaration. -/
theorem exec_additions (s : Store) (ops : List Op) (nodup : (ops.map Op.key).Nodup)
    (fits : ∀ op ∈ ops, Fits s op) :
    exec s ops = some (fun k =>
      match ops.find? (fun op => decide (op.key = k)) with
      | some op => op.result
      | none => s k) := by
  induction ops generalizing s with
  | nil => simp [exec]
  | cons op ops ih =>
    simp only [List.map_cons, List.nodup_cons] at nodup
    simp only [exec, step_fits (fits op (List.mem_cons_self ..)), Option.bind_some]
    rw [ih _ nodup.2]
    · congr 1; funext k
      by_cases hk : op.key = k
      · subst hk
        have absent : ops.find? (fun o => decide (o.key = op.key)) = none := by
          rw [List.find?_eq_none]
          intro o mem h
          exact nodup.1 (of_decide_eq_true h ▸ List.mem_map_of_mem mem)
        simp [absent, set]
      · have hk' : k ≠ op.key := fun h => hk h.symm
        simp [hk, set, hk']
    · intro o mem
      have ne : o.key ≠ op.key := fun h => nodup.1 (h ▸ List.mem_map_of_mem mem)
      exact (fits_set ne).mpr (fits o (List.mem_cons_of_mem _ mem))

theorem exec_append (s : Store) (a b : List Op) :
    exec s (a ++ b) = (exec s a).bind (exec · b) := by
  induction a generalizing s with
  | nil => simp [exec]
  | cons op a ih =>
    simp only [List.cons_append, exec]
    cases step s op <;> simp [ih]

theorem find_of_mem {l : List Op} (nodup : (l.map Op.key).Nodup) {op : Op} (mem : op ∈ l) :
    l.find? (fun o => decide (o.key = op.key)) = some op := by
  induction l with
  | nil => cases mem
  | cons x xs ih =>
    simp only [List.map_cons, List.nodup_cons] at nodup
    rw [List.find?_cons]
    cases mem with
    | head => simp
    | tail _ mem =>
      have hne : x.key ≠ op.key := fun heq => nodup.1 (heq ▸ List.mem_map_of_mem mem)
      simp only [hne, decide_false]
      exact ih nodup.2 mem

theorem find_eq_none {l : List Op} {k : Key} :
    l.find? (fun o => decide (o.key = k)) = none ↔ ∀ o ∈ l, o.key ≠ k := by
  simp [List.find?_eq_none]

section Proofs

variable {forced : Key → Bool} {old new : Program}

theorem addition_key {n : Decl} {op : Op} (h : addition forced old n = some op) :
    op.key = n.key := by
  unfold addition at h
  split at h
  · cases h; rfl
  · split at h
    · cases h; rfl
    · split at h
      · cases h
      · split at h <;> (cases h; rfl)

theorem addition_result {n : Decl} {op : Op} (h : addition forced old n = some op) :
    op.result = some n := by
  unfold addition at h
  split at h
  · cases h; rfl
  · split at h
    · cases h; rfl
    · split at h
      · cases h
      · split at h <;> (cases h; rfl)

theorem filterMap_keys_sublist (f : Decl → Option Op)
    (hf : ∀ n op, f n = some op → op.key = n.key) :
    ∀ l : List Decl, ((l.filterMap f).map Op.key).Sublist (l.map Decl.key)
  | [] => by simp
  | n :: l => by
    cases h : f n with
    | none =>
      simp only [List.filterMap_cons, h, List.map_cons]
      exact (filterMap_keys_sublist f hf l).cons _
    | some op =>
      simp only [List.filterMap_cons, h, List.map_cons, hf n op h]
      exact (filterMap_keys_sublist f hf l).cons_cons _

/-- The keys the plan removes are those `removed` says. -/
theorem mem_removals {k : Key} :
    k ∈ (old.reverse.filter (fun d => removed forced old new d.key)).map (·.key) ↔
      removed forced old new k = true := by
  simp only [List.mem_map, List.mem_filter, List.mem_reverse]
  constructor
  · rintro ⟨d, ⟨_, h⟩, rfl⟩; exact h
  · intro h
    cases hl : load old k with
    | none => simp [removed, hl] at h
    | some p =>
      obtain ⟨mem, key⟩ := load_eq_some hl
      exact ⟨p, ⟨mem, key ▸ h⟩, key⟩

/-- **Soundness.** Executed on the old program, the plan succeeds and leaves
    exactly the new program. -/
theorem plan_sound (nodupOld : (keys old).Nodup) (nodupNew : (keys new).Nodup) :
    exec (load old) (plan forced old new) = some (load new) := by
  let R := (old.reverse.filter (fun d => removed forced old new d.key)).map (·.key)
  let adds := new.filterMap (addition forced old)
  have hplan : plan forced old new = R.map .remove ++ adds := by
    simp [plan, R, adds, List.map_map, Function.comp_def]
  have nodupR : R.Nodup := by
    apply List.Nodup.sublist ((List.filter_sublist).map _)
    rw [List.map_reverse]
    exact List.pairwise_reverse.mpr (nodupOld.imp fun h e => h e.symm)
  have presentR : ∀ k ∈ R, (load old k).isSome := by
    intro k mem
    obtain ⟨d, hd, rfl⟩ := List.mem_map.mp mem
    exact load_isSome_iff.mpr
      (List.mem_map_of_mem (List.mem_reverse.mp (List.mem_filter.mp hd).1))
  let s1 : Store := fun k => if k ∈ R then none else load old k
  have mem_adds : ∀ op, op ∈ adds ↔ ∃ n ∈ new, addition forced old n = some op := by
    intro op; simp [adds, List.mem_filterMap]
  have nodupAdds : (adds.map Op.key).Nodup :=
    nodupNew.sublist (filterMap_keys_sublist _ (fun _ _ h => addition_key h) new)
  have fits : ∀ op ∈ adds, Fits s1 op := by
    intro op mem
    obtain ⟨n, memN, h⟩ := (mem_adds op).mp mem
    have hn : load new n.key = some n := load_of_mem nodupNew memN
    unfold addition at h
    split at h
    · rename_i hold
      cases h
      simp [Fits, s1, hold]
    · rename_i p hold
      have inR : forced n.key = true ∨ (p ≠ n ∧ n.key.1 ≠ .reaction) → n.key ∈ R := by
        intro c
        apply mem_removals.mpr
        simp only [removed, hold, hn]
        rcases c with c | ⟨c1, c2⟩
        · simp [c]
        · simp [c1, c2]
      split at h
      · rename_i hf
        cases h
        simp [Fits, s1, inR (Or.inl hf)]
      · split at h
        · cases h
        · rename_i hne
          split at h
          · rename_i hr
            cases h
            have notR : n.key ∉ R := by
              intro m
              have := mem_removals.mp m
              simp [removed, hold, hn, hr] at this
              simp_all
            simp [Fits, s1, notR, hold]
          · rename_i hr
            cases h
            simp [Fits, s1, inR (Or.inr ⟨hne, hr⟩)]
  rw [hplan, exec_append, exec_removals _ R nodupR presentR, Option.bind_some,
    exec_additions s1 adds nodupAdds fits]
  congr 1
  funext k
  cases hk : load new k with
  | some n =>
    obtain ⟨memN, rfl⟩ := load_eq_some hk
    cases ha : addition forced old n with
    | some op =>
      have memOp : op ∈ adds := (mem_adds op).mpr ⟨n, memN, ha⟩
      have key := addition_key ha
      rw [← key, find_of_mem nodupAdds memOp]
      exact addition_result ha
    | none =>
      have absent : adds.find? (fun o => decide (o.key = n.key)) = none := by
        refine find_eq_none.mpr fun o mem hkey => ?_
        obtain ⟨n', memN', h'⟩ := (mem_adds o).mp mem
        have : n' = n := by
          have e1 := load_of_mem nodupNew memN'
          rw [← addition_key h', hkey, hk] at e1
          exact (Option.some.inj e1).symm
        subst this
        simp [ha] at h'
      rw [absent]
      unfold addition at ha
      split at ha
      · simp at ha
      · rename_i p hold
        split at ha
        · simp at ha
        · split at ha
          · rename_i heq
            have notR : n.key ∉ R := by
              intro m
              have := mem_removals.mp m
              simp_all [removed]
            simp [s1, notR, hold, heq]
          · split at ha <;> simp at ha
  | none =>
    have absent : adds.find? (fun o => decide (o.key = k)) = none := by
      refine find_eq_none.mpr fun o mem hkey => ?_
      obtain ⟨n', memN', h'⟩ := (mem_adds o).mp mem
      have e1 := load_of_mem nodupNew memN'
      rw [← addition_key h', hkey, hk] at e1
      cases e1
    rw [absent]
    show (if k ∈ R then none else load old k) = none
    split
    · rfl
    · rename_i notR
      cases hold : load old k with
      | none => rfl
      | some p =>
        exact absurd (mem_removals.mpr (by simp [removed, hold, hk])) notR

/-- **Minimality.** A declaration the same in both programs, and not one the
    caller replaces, is not touched by the plan: a retained component is never
    restarted for nothing. -/
theorem plan_minimal (nodupNew : (keys new).Nodup) {k : Key} {d : Decl}
    (hold : load old k = some d) (hnew : load new k = some d) (keep : forced k = false) :
    ∀ op ∈ plan forced old new, op.key ≠ k := by
  intro op mem hkey
  simp only [plan, List.mem_append, List.mem_map, List.mem_filterMap] at mem
  rcases mem with ⟨d', hd', rfl⟩ | ⟨n, memN, h⟩
  · have := (List.mem_filter.mp hd').2
    simp only [Op.key] at hkey
    simp [removed, hkey, hold, hnew, keep] at this
  · have e := load_of_mem nodupNew memN
    rw [← addition_key h, hkey, hnew] at e
    cases e
    simp [addition, (load_eq_some hnew).2, hold, keep] at h

end Proofs

/-! ## Bytecode

A plan reaches the runtime as instructions. A removal names its kind in its
opcode (`kill_agent`, `remove_port`, …) and the component by name; a
declaration is the instruction the program itself would declare it with.
`vm` is what the runtime does with them. -/

inductive Instr where
  | removal (op : String) (name : String)
  | declare (d : Decl)
  | update (d : Decl)
  deriving DecidableEq, Repr

def removalOp : Kind → String
  | .type => "remove_type"
  | .inst => "remove_instance"
  | .agent => "kill_agent"
  | .port => "remove_port"
  | .timer => "remove_timer"
  | .state => "remove_state"
  | .param => "remove_param"
  | .connection => "disconnect_ports"
  | .reaction => "remove_reaction"

/-- How the runtime reads a removal's opcode back as the kind it removes. -/
def removalKind : String → Option Kind
  | "remove_type" => some .type
  | "remove_instance" => some .inst
  | "kill_agent" => some .agent
  | "remove_port" => some .port
  | "remove_timer" => some .timer
  | "remove_state" => some .state
  | "remove_param" => some .param
  | "disconnect_ports" => some .connection
  | "remove_reaction" => some .reaction
  | _ => none

def emit : Op → Instr
  | .remove (kind, name) => .removal (removalOp kind) name
  | .declare d => .declare d
  | .update d => .update d

def vmStep (s : Store) : Instr → Option Store
  | .removal op name => (removalKind op).bind fun kind => step s (.remove (kind, name))
  | .declare d => step s (.declare d)
  | .update d => step s (.update d)

def vm (s : Store) : List Instr → Option Store
  | [] => some s
  | i :: is => (vmStep s i).bind (vm · is)

theorem removalKind_removalOp (kind : Kind) : removalKind (removalOp kind) = some kind := by
  cases kind <;> rfl

/-- **Code generation is sound.** The emitted bytecode, run by the bytecode's
    semantics, does exactly what the plan does. -/
theorem emit_sound (s : Store) (ops : List Op) : vm s (ops.map emit) = exec s ops := by
  induction ops generalizing s with
  | nil => rfl
  | cons op ops ih =>
    simp only [List.map_cons, vm, exec]
    have : vmStep s (emit op) = step s op := by
      cases op with
      | remove k =>
        obtain ⟨kind, name⟩ := k
        simp [emit, vmStep, removalKind_removalOp]
      | declare d => rfl
      | update d => rfl
    rw [this]
    cases step s op <;> simp [ih]

/-- **End to end.** The bytecode emitted for a plan turns the old program into
    the new one. -/
theorem compiled_plan_sound (forced : Key → Bool) {old new : Program}
    (nodupOld : (keys old).Nodup) (nodupNew : (keys new).Nodup) :
    vm (load old) ((plan forced old new).map emit) = some (load new) := by
  rw [emit_sound]
  exact plan_sound nodupOld nodupNew

end Omar.Mutation
