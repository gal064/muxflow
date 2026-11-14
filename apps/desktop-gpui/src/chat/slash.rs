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

/// The name being completed: a command or skill after a leading slash, or a
/// skill after a `$` anywhere in the message.
#[derive(Debug, PartialEq, Eq)]
pub struct Query<'a> {
    /// Named with `$`: only skills are offered.
    pub skill: bool,
    pub word: &'a str,
    /// Where the name's sigil is in the text.
    pub start: usize,
}

impl Query<'_> {
    pub fn sigil(&self) -> char {
        if self.skill { '$' } else { '/' }
    }
}

/// What is being completed with the cursor at `cursor`. A slash counts only
/// while it is the whole text: after a space the command is chosen and its
/// arguments are being typed. A `$` counts in the word the cursor ends.
pub fn query(text: &str, cursor: usize) -> Option<Query<'_>> {
    if let Some(word) = text.strip_prefix('/')
        && !word.contains(char::is_whitespace)
    {
        return Some(Query {
            skill: false,
            word,
            start: 0,
        });
    }
    let before = text.get(..cursor)?;
    let start = before
        .char_indices()
        .rev()
        .find(|(_, character)| character.is_whitespace())
        .map_or(0, |(at, character)| at + character.len_utf8());
    let word = before[start..].strip_prefix('$')?;
    Some(Query {
        skill: true,
        word,
        start,
    })
}

/// What the query offers: commands, then skills, each best match first and in
/// the catalog's order where equal.
pub fn suggestions<'a>(catalog: &'a ChatCatalog, query: &Query) -> Vec<&'a ChatCommand> {
    // An agent that has not said yet which of its commands are skills is
    // asked for any of them.
    let marked = catalog
        .commands
        .iter()
        .any(|command| section(command) == Section::Skills);
    let offered = |command: &&ChatCommand| match kind(command) {
        _ if !query.skill => true,
        ChatCommandKind::Skill => true,
        ChatCommandKind::Muxflow => false,
        _ => !marked,
    };
    let mut ranked: Vec<(Section, i64, &ChatCommand)> = catalog
        .commands
        .iter()
        .filter(offered)
        .filter_map(|command| {
            let score = fuzzy_match(&command.name, query.word)?.score;
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
        let text = format!("/{query}");
        suggestions(&catalog(), &super::query(&text, text.len()).unwrap())
            .into_iter()
            .map(|command| command.name.clone())
            .collect()
    }

    fn word(text: &str) -> Option<&str> {
        query(text, text.len()).map(|query| query.word)
    }

    #[test]
    fn the_list_opens_on_a_slash_and_closes_at_the_first_space() {
        assert_eq!(word("/"), Some(""));
        assert_eq!(word("/co"), Some("co"));
        assert_eq!(word("/compact "), None);
        assert_eq!(word("/co\nmore"), None);
        assert_eq!(word("see /co"), None);
        assert_eq!(word(""), None);
    }

    #[test]
    fn a_dollar_names_a_skill_wherever_the_cursor_is() {
        let skill = |word, start| {
            Some(Query {
                skill: true,
                word,
                start,
            })
        };
        assert_eq!(query("$", 1), skill("", 0));
        assert_eq!(query("run $co", 7), skill("co", 4));
        assert_eq!(query("run $code now", 7), skill("co", 4));
        assert_eq!(query("/compact $co", 12), skill("co", 9));
        assert_eq!(query("run $code now", 13), None);
        assert_eq!(query("costs $5 ", 9), None);
        let text = "run $c";
        let marked = catalog();
        let names: Vec<_> = suggestions(&marked, &query(text, text.len()).unwrap())
            .into_iter()
            .map(|command| command.name.as_str())
            .collect();
        assert_eq!(names, ["code-review", "diagnose-crash"]);

        // No skill is marked: the agent's own commands stand in.
        let mut unmarked = catalog();
        unmarked
            .commands
            .retain(|command| kind(command) != ChatCommandKind::Skill);
        let names: Vec<_> = suggestions(&unmarked, &query(text, text.len()).unwrap())
            .into_iter()
            .map(|command| command.name.as_str())
            .collect();
        assert_eq!(names, ["compact", "clear"]);
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
