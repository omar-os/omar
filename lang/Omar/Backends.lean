-- Generated from src/backend/mod.rs; do not edit. Regenerate with
--     UPDATE_BACKENDS=1 cargo test --bin omar the_lean_backends_are_current

namespace Omar.Backends

/-- The backends an agent with a `per` lifetime may name, each by its aliases,
    its name first: those whose `fresh_conversation` is true. -/
def freshConversation : Array (Array String) := #[
  #["claude", "claude-code", "claude_code", "claudecode"],
  #["codex"],
  #["opencode"],
  #["pi"],
  #["jev"],
  #["stub"]
]

/-- The backends an agent may give a model, likewise: those whose
    `model_flag` is some. -/
def modelSelectable : Array (Array String) := #[
  #["claude", "claude-code", "claude_code", "claudecode"],
  #["codex"],
  #["cursor"],
  #["opencode"],
  #["pi"],
  #["agy", "antigravity"],
  #["jev"]
]

end Omar.Backends
