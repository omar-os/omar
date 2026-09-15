//! Registry of approval notification capabilities, independent of launch policy.
mod codex;
mod terminal;
use super::{ApprovalObserver, ApprovalTarget};

struct Backend {
    name: &'static str,
    create: fn(ApprovalTarget) -> Box<dyn ApprovalObserver>,
}

const BACKENDS: &[Backend] = &[
    Backend {
        name: "codex",
        create: codex::CodexObserver::from_target,
    },
    Backend {
        name: "claude",
        create: terminal::TerminalObserver::claude_from_target,
    },
    Backend {
        name: "cursor",
        create: terminal::TerminalObserver::cursor_from_target,
    },
    Backend {
        name: "agy",
        create: terminal::TerminalObserver::antigravity_from_target,
    },
];

pub(super) fn observer(target: ApprovalTarget) -> Option<Box<dyn ApprovalObserver>> {
    BACKENDS
        .iter()
        .find(|backend| backend.name == target.backend)
        .map(|backend| (backend.create)(target))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_advertises_implemented_observers() {
        for (backend, supported) in [
            ("codex", true),
            ("claude", true),
            ("cursor", true),
            ("agy", true),
            ("unknown", false),
        ] {
            assert_eq!(
                observer(ApprovalTarget {
                    backend: backend.into(),
                    session: "unused".into(),
                    command: None
                })
                .is_some(),
                supported,
                "{backend}"
            );
        }
    }
}
