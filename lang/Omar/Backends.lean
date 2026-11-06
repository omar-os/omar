-- Generated from src/backend/mod.rs; do not edit. Regenerate with
--     UPDATE_BACKENDS=1 cargo test --bin omar the_lean_backends_are_current

namespace Omar.Backends

/-- The backends an invocation-scoped agent may name, each by its aliases,
    its name first: those whose `fresh_conversation` is true. -/
def freshConversation : Array (Array String) := #[
  #["claude", "claude-code", "claude_code", "claudecode"],
  #["codex"],
  #["cursor"],
  #["opencode"],
  #["pi"],
  #["jev"],
  #["stub"]
]

end Omar.Backends
