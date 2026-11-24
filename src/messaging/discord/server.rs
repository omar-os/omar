//! Server messages share one runtime conversation per Discord thread.
use super::*;
use crate::messaging::{config::DiscordServer, ServerContext};

#[derive(Clone)]
pub(super) struct ServerRouter {
    pub config: DiscordServer,
    pub users: Vec<String>,
    pub bot: String,
    pub api: Arc<DiscordApi>,
    pub seen: Arc<Mutex<HashMap<String, Instant>>>,
}
impl ServerRouter {
    pub fn accepts(&self, event: &Value, interaction: bool) -> bool {
        let user = if interaction {
            &event["member"]["user"]
        } else {
            &event["author"]
        };
        event["guild_id"] == self.config.guild_id
            && user["bot"] != true
            && event.get("webhook_id").is_none()
            && id(&user["id"]).is_some_and(|sender| self.users.iter().any(|u| u == sender))
            && id(&event["channel_id"]).is_some()
            && if interaction {
                event["application_id"] == self.bot && matches!(event["type"].as_u64(), Some(2 | 3))
            } else {
                event["type"] == 0
                    && event["content"].as_str().is_some_and(|text| {
                        text.contains(&format!("<@{}>", self.bot))
                            || text.contains(&format!("<@!{}>", self.bot))
                    })
            }
    }

    pub async fn route(&self, event: &Value, interaction: bool) -> Result<Option<Incoming>> {
        if !self.accepts(event, interaction) {
            return Ok(None);
        }
        let channel = id(&event["channel_id"]).context("Missing channel")?;
        let metadata = self
            .api
            .call(reqwest::Method::GET, &format!("/channels/{channel}"), None)
            .await?;
        let Some(parent) = approved_parent(&self.config, channel, &metadata) else {
            return Ok(None);
        };
        let mut normalized = event.clone();
        normalized.as_object_mut().unwrap().remove("guild_id");
        if interaction {
            normalized["user"] = event["member"]["user"].clone();
        }
        let sender = if interaction {
            id(&normalized["user"]["id"])
        } else {
            id(&normalized["author"]["id"])
        }
        .context("Missing sender")?
        .to_owned();
        if !interaction {
            normalized["content"] = json!(event["content"]
                .as_str()
                .unwrap_or("")
                .replace(&format!("<@{}>", self.bot), "")
                .replace(&format!("<@!{}>", self.bot), "")
                .trim());
        }
        let channels = HashMap::from([(sender, channel.to_owned())]);
        let message = if interaction {
            normalize_interaction(&normalized, &self.bot, &channels)
        } else {
            normalize(&normalized, &self.bot, &channels)
        };
        let Some(mut message) = message else {
            return Ok(None);
        };
        if message.command.is_empty() {
            return Ok(None);
        }
        // Claim before creating a thread: a replay must not create another channel.
        {
            let mut seen = self.seen.lock().await;
            let now = Instant::now();
            seen.retain(|_, expires| *expires > now);
            if seen.contains_key(&message.event_id) || seen.len() >= 2000 {
                return Ok(None);
            }
            seen.insert(message.event_id.clone(), now + Duration::from_secs(3600));
        }
        // A top-level request starts a public thread. Channel permission overwrites
        // still control visibility; no channel content is copied to another channel.
        if metadata["type"] == 0 {
            if interaction && event["type"] == 3 {
                return Ok(None);
            }
            let path = if interaction {
                format!("/channels/{channel}/threads")
            } else {
                format!("/channels/{channel}/messages/{}/threads", message.event_id)
            };
            let thread = self
                .api
                .call(
                    reqwest::Method::POST,
                    &path,
                    Some(
                        json!({"name":"OMAR conversation", "auto_archive_duration":1440,"type":11}),
                    ),
                )
                .await?;
            ensure!(
                thread["guild_id"] == self.config.guild_id
                    && thread["parent_id"] == channel
                    && thread["type"] == 11,
                "Unexpected Discord thread"
            );
            message.destination.conversation_id =
                id(&thread["id"]).context("Missing thread ID")?.into();
            message.destination.thread_id = None;
        }
        message.server_context = Some(ServerContext {
            guild_id: self.config.guild_id.clone(),
            parent_id: parent,
        });
        Ok(Some(message))
    }
}

fn approved_parent(config: &DiscordServer, channel: &str, metadata: &Value) -> Option<String> {
    if metadata["id"] != channel || metadata["guild_id"] != config.guild_id {
        return None;
    }
    let parent = match metadata["type"].as_u64()? {
        0 => channel,
        11 | 12 => id(&metadata["parent_id"])?,
        _ => return None,
    };
    config
        .channels
        .iter()
        .any(|c| c == parent)
        .then(|| parent.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> DiscordServer {
        DiscordServer {
            guild_id: "111".into(),
            channels: vec!["456".into()],
        }
    }
    #[test]
    fn channel_metadata_must_match_server_and_approved_parent() {
        let c = config();
        assert_eq!(
            approved_parent(&c, "456", &json!({"id":"456","guild_id":"111","type":0})),
            Some("456".into())
        );
        for kind in [11, 12] {
            assert_eq!(
                approved_parent(
                    &c,
                    "789",
                    &json!({"id":"789","guild_id":"111","type":kind,"parent_id":"456"})
                ),
                Some("456".into())
            );
        }
        for metadata in [
            json!({"id":"789","guild_id":"222","type":11,"parent_id":"456"}),
            json!({"id":"789","guild_id":"111","type":11,"parent_id":"000"}),
            json!({"id":"789","guild_id":"111","type":0,"parent_id":"456"}),
            json!({"id":"789","guild_id":"111","type":4,"parent_id":"456"}),
            json!({"id":"456","guild_id":"111","type":11,"parent_id":"456"}),
        ] {
            assert!(approved_parent(&c, "789", &metadata).is_none());
        }
    }
    #[test]
    fn server_requires_authorized_human_mentions_or_native_interactions() {
        let router = ServerRouter {
            config: config(),
            users: vec!["123".into()],
            bot: "999".into(),
            api: Arc::new(DiscordApi::new("fixture".into()).unwrap()),
            seen: Default::default(),
        };
        let mut event = json!({"guild_id":"111","type":0,"channel_id":"456","author":{"id":"123"},"content":"<@999> hello"});
        assert!(router.accepts(&event, false));
        event["content"] = json!("ordinary discussion");
        assert!(!router.accepts(&event, false));
        event["content"] = json!("<@999> hello");
        event["author"]["id"] = json!("124");
        assert!(!router.accepts(&event, false));
        event["author"]["id"] = json!("123");
        event["author"]["bot"] = json!(true);
        assert!(!router.accepts(&event, false));
        let mut event = json!({"guild_id":"111","application_id":"999","type":2,"channel_id":"456","member":{"user":{"id":"123"}}});
        assert!(router.accepts(&event, true));
        event["guild_id"] = json!("222");
        assert!(!router.accepts(&event, true));
    }
}
