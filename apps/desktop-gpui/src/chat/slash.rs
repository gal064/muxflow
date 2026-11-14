//! The composer's slash list: which of the provider's commands and skills a
//! typed `/word` offers, and which sent message is Muxflow's own side question.
//! The list is the chat's catalog; nothing is named here.

use desktop_core::connection::chat::{ChatCatalog, ChatCommand, ChatCommandKind};

use crate::commands::fuzzy::fuzzy_match;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Commands,
    Skills,
}

impl Section {
    pub fn title(self) -> &'static str {
        match self {
            Self::Commands => "Commands",
            Self::Skills => "Skills",
        }
    }
}

fn kind(command: &ChatCommand) -> ChatCommandKind {
    ChatCommandKind::try_from(command.kind).unwrap_or_default()
}

pub fn section(command: &ChatCommand) -> Section {
    match kind(command) {
        ChatCommandKind::Skill => Section::Skills,
        _ => Section::Commands,
    }
}

/// The word being typed after a leading slash, while it is the whole text.
/// `None` once there is a space: the command is chosen and its arguments
/// are being typed.
pub fn query(text: &str) -> Option<&str> {
    let word = text.strip_prefix('/')?;
    (!word.contains(char::is_whitespace)).then_some(word)
}

/// What `/query` offers: commands, then skills, each best match first and in
/// the catalog's order where equal.
pub fn suggestions<'a>(catalog: &'a ChatCatalog, query: &str) -> Vec<&'a ChatCommand> {
    let mut ranked: Vec<(Section, i64, &ChatCommand)> = catalog
        .commands
        .iter()
        .filter_map(|command| {
            let score = fuzzy_match(&command.name, query)?.score;
            Some((section(command), score, command))
        })
        .collect();
    ranked.sort_by_key(|(section, score, _)| {
        (*section == Section::Skills, std::cmp::Reverse(*score))
    });
    ranked.into_iter().map(|(_, _, command)| command).collect()
}

/// The question of a message that starts with one of Muxflow's own commands
/// (`/btw`, `/side`), which forks instead of being sent.
pub fn side_question<'a>(catalog: &ChatCatalog, text: &'a str) -> Option<&'a str> {
    let rest = text.strip_prefix('/')?;
    let (name, question) = match rest.split_once(char::is_whitespace) {
        Some((name, question)) => (name, question.trim()),
        None => (rest, ""),
    };
    catalog
        .commands
        .iter()
        .any(|command| kind(command) == ChatCommandKind::Muxflow && command.name == name)
        .then_some(question)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> ChatCatalog {
        let command = |name: &str, kind: ChatCommandKind| ChatCommand {
            name: name.into(),
            kind: kind.into(),
            ..Default::default()
        };
        ChatCatalog {
            commands: vec![
                command("code-review", ChatCommandKind::Skill),
                command("compact", ChatCommandKind::Command),
                command("clear", ChatCommandKind::Command),
                command("btw", ChatCommandKind::Muxflow),
                command("diagnose-crash", ChatCommandKind::Skill),
            ],
            ..Default::default()
        }
    }

    fn names(query: &str) -> Vec<String> {
        suggestions(&catalog(), query)
            .into_iter()
            .map(|command| command.name.clone())
            .collect()
    }

    #[test]
    fn the_list_opens_on_a_slash_and_closes_at_the_first_space() {
        assert_eq!(query("/"), Some(""));
        assert_eq!(query("/co"), Some("co"));
        assert_eq!(query("/compact "), None);
        assert_eq!(query("/co\nmore"), None);
        assert_eq!(query("see /co"), None);
        assert_eq!(query(""), None);
    }

    #[test]
    fn commands_come_before_skills_in_the_catalogs_order() {
        assert_eq!(
            names(""),
            ["compact", "clear", "btw", "code-review", "diagnose-crash"]
        );
    }

    #[test]
    fn a_query_filters_by_name_and_keeps_the_sections() {
        assert_eq!(names("co"), ["compact", "code-review"]);
        assert_eq!(names("cr"), ["clear", "code-review", "diagnose-crash"]);
        assert!(names("zz").is_empty());
    }

    #[test]
    fn only_muxflows_own_commands_fork() {
        let catalog = catalog();
        assert_eq!(
            side_question(&catalog, "/btw which file holds the join?"),
            Some("which file holds the join?")
        );
        assert_eq!(side_question(&catalog, "/btw"), Some(""));
        assert_eq!(side_question(&catalog, "/compact now"), None);
        assert_eq!(side_question(&catalog, "/btwx hello"), None);
        assert_eq!(side_question(&catalog, "by the way /btw"), None);
    }
}
