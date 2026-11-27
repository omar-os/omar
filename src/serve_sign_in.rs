//! Notice an expired Claude sign-in in any agent this session runs, tell the
//! operator once, and continue the stuck agents once the sign-in works again.
//! Every Claude process shares one credential, so when it expires they all stop
//! at `Login expired · Please run /login` and otherwise look busy forever.

use super::*;

/// How often panes are read.
const SCAN: Duration = Duration::from_secs(15);
/// While agents are stuck, how many scans pass between sign-in checks. Each is a
/// one-line `claude -p`, so not every scan.
const CHECK_EVERY: u32 = 4;
const CAPTURE_LINES: i32 = 35;
/// What a stuck agent is sent once the sign-in works again.
pub(super) const CONTINUE: &str =
    "Your Claude sign-in was renewed. Continue the task you were working on.";

/// Whether Claude's latest status line says its sign-in expired. Only the
/// newest `⏺` line counts, so a quoted or old error, or one the agent has since
/// recovered from, is not a blocker.
pub(super) fn login_expired(screen: &str) -> bool {
    let Some(latest) = screen
        .lines()
        .rev()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("⏺ "))
    else {
        return false;
    };
    latest.contains("/login")
        && (latest.starts_with("Login expired")
            || (latest.starts_with("API Error: 401") && latest.contains("OAuth token has expired")))
}

/// What the watcher wants done after one scan.
#[derive(Debug, PartialEq)]
pub(super) enum Step {
    /// Tell the operator these agents are stuck (first sight only).
    Stuck(Vec<String>),
    /// Run a sign-in check, and report it with [`Watch::signed_in`].
    Check,
}

/// Which Claude sessions are stuck. Kept apart from tmux so it can be tested.
#[derive(Default)]
pub(super) struct Watch {
    stuck: BTreeSet<String>,
    told: BTreeSet<String>,
    /// Stuck sessions a continue could not reach, said once each.
    unreachable: BTreeSet<String>,
    scans: u32,
}

impl Watch {
    /// One scan: each live Claude session with its screen.
    pub(super) fn scan<'a>(
        &mut self,
        screens: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Vec<Step> {
        self.stuck = screens
            .into_iter()
            .filter(|(_, screen)| login_expired(screen))
            .map(|(name, _)| name.to_string())
            .collect();
        // A session that recovered (or ended) may be told again next time.
        self.told.retain(|name| self.stuck.contains(name));
        self.unreachable.retain(|name| self.stuck.contains(name));
        let mut steps = Vec::new();
        let new: Vec<String> = self.stuck.difference(&self.told).cloned().collect();
        if !new.is_empty() {
            self.told.extend(new.iter().cloned());
            steps.push(Step::Stuck(new));
        }
        if self.stuck.is_empty() {
            self.scans = 0;
        } else {
            self.scans += 1;
            if self.scans.is_multiple_of(CHECK_EVERY) {
                steps.push(Step::Check);
            }
        }
        steps
    }

    /// The sign-in check passed: the sessions to continue. The operator is not
    /// told about them again until one recovers and gets stuck anew, so a
    /// continue that does not land (or lands slowly) is retried at the next
    /// check instead of repeating the notice.
    pub(super) fn signed_in(&self) -> Vec<String> {
        self.stuck.iter().cloned().collect()
    }

    /// A continue could not reach `name`: true the first time, to say so once.
    pub(super) fn unreachable(&mut self, name: &str) -> bool {
        self.unreachable.insert(name.to_string())
    }
}

pub(super) fn stuck_notice(names: &[String]) -> String {
    format!(
        "**Claude's sign-in expired.** {} stopped and will wait until you sign in again: {}. \
         Run `omar connect claude --replace`, or type `/login` in any Claude pane. \
         OMAR continues them once the sign-in works.",
        if names.len() == 1 {
            "One agent"
        } else {
            "These agents"
        },
        names.join(", ")
    )
}

pub(super) fn continued_notice(names: &[String]) -> String {
    format!(
        "**Claude is signed in again.** Continued: {}.",
        names.join(", ")
    )
}

/// Watch every Claude session under this OMAR session's prefix, EAs, workers
/// and topology agents alike, until the server shuts down.
pub(super) fn watch(context: Arc<Context_>) {
    if cfg!(test) {
        return;
    }
    thread::spawn(move || {
        let client = TmuxClient::new(context.session_prefix.clone());
        let mut watch = Watch::default();
        while !context.shutdown.load(Ordering::SeqCst) {
            thread::sleep(SCAN);
            let screens: Vec<(String, String)> = client
                .list_sessions()
                .unwrap_or_default()
                .into_iter()
                .filter(|s| client.session_backend(&s.name).as_deref() == Some("claude"))
                .filter_map(|s| {
                    let screen = client.capture_pane_plain(&s.name, CAPTURE_LINES).ok()?;
                    Some((s.name, screen))
                })
                .collect();
            for step in watch.scan(screens.iter().map(|(n, s)| (n.as_str(), s.as_str()))) {
                match step {
                    Step::Stuck(names) => {
                        let text = stuck_notice(&names);
                        eprintln!("OMAR: {text}");
                        let _ = context.publish_runtime(text);
                    }
                    Step::Check => {
                        let claude = crate::backend::of(crate::backend::Kind::Claude);
                        if crate::backend::connection::check_sign_in(claude).is_err() {
                            continue;
                        }
                        let names: Vec<String> = watch
                            .signed_in()
                            .into_iter()
                            .filter(|name| {
                                match client.deliver_prompt(
                                    name,
                                    CONTINUE,
                                    &DeliveryOptions::default(),
                                ) {
                                    Ok(()) => true,
                                    Err(error) => {
                                        if watch.unreachable(name) {
                                            eprintln!("OMAR: could not continue {name}: {error:#}");
                                        }
                                        false
                                    }
                                }
                            })
                            .collect();
                        if !names.is_empty() {
                            let text = continued_notice(&names);
                            eprintln!("OMAR: {text}");
                            let _ = context.publish_runtime(text);
                        }
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPIRED: &str = "⏺ Working on it\n⏺ Login expired · Please run /login\n❯";

    #[test]
    fn only_the_latest_status_line_counts() {
        assert!(login_expired(EXPIRED));
        assert!(login_expired(
            "⏺ API Error: 401 {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\
             \"message\":\"OAuth token has expired. Please obtain a new token or refresh your \
             existing token.\"}} · Please run /login\n❯"
        ));
        assert!(!login_expired(
            "The log says Login expired · Please run /login"
        ));
        assert!(!login_expired(&format!("{EXPIRED}\n⏺ Continuing now")));
        assert!(!login_expired(
            "⏺ API Error: 500 overloaded · Please run /login"
        ));
        assert!(!login_expired("Working for 60s without output"));
    }

    #[test]
    fn stuck_agents_are_reported_once_checked_and_continued_once() {
        let mut watch = Watch::default();
        let a = ("omar-agent-0-a", EXPIRED);
        let b = ("omar-agent-0-b", EXPIRED);
        let fine = ("omar-agent-0-c", "⏺ Done\n❯");
        assert_eq!(
            watch.scan([a, fine]),
            vec![Step::Stuck(vec!["omar-agent-0-a".into()])]
        );
        // Seen again: no second notice; a newly stuck one is reported alone.
        assert_eq!(
            watch.scan([a, b, fine]),
            vec![Step::Stuck(vec!["omar-agent-0-b".into()])]
        );
        assert!(watch.scan([a, b]).is_empty());
        assert_eq!(watch.scan([a, b]), vec![Step::Check]);
        assert_eq!(watch.signed_in(), vec!["omar-agent-0-a", "omar-agent-0-b"]);
        // Still on screen after the continue (slow, or it did not land): no new
        // notice; the next check retries the continue.
        for _ in 0..3 {
            assert!(watch.scan([a, b]).is_empty());
        }
        assert_eq!(watch.scan([a, b]), vec![Step::Check]);
        // Once it recovers and gets stuck again, the operator is told again.
        watch.scan([("omar-agent-0-a", "⏺ Back at it")]);
        assert_eq!(
            watch.scan([a]),
            vec![Step::Stuck(vec!["omar-agent-0-a".into()])]
        );
    }

    #[test]
    fn an_unreachable_agent_is_said_once_per_episode() {
        let mut watch = Watch::default();
        let a = ("omar-agent-0-a", EXPIRED);
        watch.scan([a]);
        assert!(watch.unreachable("omar-agent-0-a"));
        assert!(!watch.unreachable("omar-agent-0-a"));
        watch.scan([("omar-agent-0-a", "⏺ Back at it")]);
        watch.scan([a]);
        assert!(watch.unreachable("omar-agent-0-a"));
    }

    #[test]
    fn a_recovered_agent_stops_the_checks() {
        let mut watch = Watch::default();
        let a = ("omar-agent-0-a", EXPIRED);
        watch.scan([a]);
        for _ in 0..10 {
            assert!(!watch
                .scan([("omar-agent-0-a", "⏺ Back at it")])
                .contains(&Step::Check));
        }
    }

    #[test]
    fn notices_name_the_agents_and_the_way_back() {
        let text = stuck_notice(&["omar-agent-0-a".into()]);
        assert!(text.contains("omar connect claude --replace") && text.contains("/login"));
        assert!(text.contains("omar-agent-0-a"));
    }
}
