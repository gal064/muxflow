//! Starting one run: the automation workspace, a new tab in it, and the
//! command typed into that tab's shell.

use std::{path::Path, process::Command, thread::sleep, time::Duration};

use anyhow::{Context, bail};
use tmux_agent_protocol::v1;

use super::store::{Agent, Definition};
use crate::service::{
    terminal::{InputDelivery, send_input_batch},
    tmux_actions::{
        APP_SHELL, configure_new_session, escaped_format_literal, resolved_start_directory, run,
        run_for_ids, validate_name,
    },
};

pub(super) type Tmux = dyn Fn() -> anyhow::Result<Command>;

/// Fully unattended defaults. Anything else is a custom command.
const CLAUDE_COMMAND: &str = "claude --dangerously-skip-permissions";
const CODEX_COMMAND: &str = "codex --dangerously-bypass-approvals-and-sandbox";

/// Marks the one session runs open in. A tag rather than a name, so renaming
/// the workspace does not make the helper create another.
pub(super) const WORKSPACE_TAG: &str = "@muxflow-automation-workspace";
const WORKSPACE_NAME: &str = "automation";
pub(super) const AUTOMATION_TAG: &str = "@muxflow-automation";
pub(super) const RUN_TAG: &str = "@muxflow-automation-run";
/// Tells this app's own window-naming hook to leave the window's name alone.
const NAME_LOCK_TAG: &str = "@muxflow-name-locked";

const PROMPT_WAIT_STEP: Duration = Duration::from_millis(100);
const PROMPT_WAIT_STEPS: u32 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Launched {
    pub session_id: String,
    pub window_id: String,
    pub pane_id: String,
}

/// The line typed into the tab: the command, then the prompt as one argument.
/// The default agents get `--` before it, so a prompt that starts with `-` is
/// not read as an option.
///
/// The prompt is read from a file rather than quoted inline, so a multi-line
/// prompt full of quotes reaches the command intact in bash, zsh and fish
/// alike. The path is single-quoted; fish treats `\` inside single quotes as
/// an escape, so a path with `'` or `\` is refused rather than escaped.
pub(super) fn command_line(definition: &Definition, prompt_file: &Path) -> anyhow::Result<String> {
    let path = prompt_file
        .to_str()
        .context("the prompt file path is not UTF-8")?;
    if path.contains(['\'', '\\']) || path.chars().any(char::is_control) {
        bail!("the prompt file path {path:?} cannot be quoted for every shell");
    }
    let program = match definition.agent {
        Agent::Claude => format!("{CLAUDE_COMMAND} --"),
        Agent::Codex => format!("{CODEX_COMMAND} --"),
        Agent::Custom => definition.command.clone(),
    };
    Ok(format!("{program} \"$(cat '{path}')\""))
}

/// Opens the run's tab and types its command. The tab opens in the
/// background, so whoever is looking at the workspace keeps their focus.
pub(super) fn launch(
    tmux: &Tmux,
    definition: &Definition,
    number: u32,
    prompt_file: &Path,
    embedded_codex: bool,
) -> anyhow::Result<Launched> {
    let cwd = resolved_start_directory(&definition.cwd)?;
    let name = format!("{} #{number}", definition.name);
    validate_name(&name)?;
    let line = command_line(definition, prompt_file)?;

    let (session_id, fresh) = ensure_workspace(tmux, &definition.cwd, embedded_codex)?;
    let (window_id, pane_id) = match fresh {
        // A workspace created just now already has a tab in the run's
        // directory; use it rather than leave an idle shell behind.
        Some(ids) => ids,
        None => {
            let mut command = tmux()?;
            command.args([
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{window_id} #{pane_id}",
                "-t",
                &format!("{session_id}:"),
                "-c",
                &escaped_format_literal(&cwd),
                APP_SHELL,
            ]);
            let ids = run_for_ids(command, &['@', '%'])?;
            (ids[0].clone(), ids[1].clone())
        }
    };

    // A tab that cannot be named and typed into would sit idle and untagged,
    // where retention never finds it.
    if let Err(error) = tag_and_type(tmux, definition, &window_id, &pane_id, &name, number, &line) {
        if let Ok(mut kill) = tmux() {
            let _ = kill.args(["kill-window", "-t", &window_id]).output();
        }
        return Err(error);
    }
    Ok(Launched {
        session_id,
        window_id,
        pane_id,
    })
}

fn tag_and_type(
    tmux: &Tmux,
    definition: &Definition,
    window_id: &str,
    pane_id: &str,
    name: &str,
    number: u32,
    line: &str,
) -> anyhow::Result<()> {
    let number = number.to_string();
    let escaped_name = escaped_format_literal(name);
    let mut tag = tmux()?;
    tag.args(["rename-window", "-t", window_id, &escaped_name]);
    for (option, value) in [
        (AUTOMATION_TAG, definition.id.as_str()),
        (RUN_TAG, number.as_str()),
        (NAME_LOCK_TAG, "1"),
        ("automatic-rename", "off"),
        ("allow-rename", "off"),
    ] {
        tag.args([";", "set-option", "-w", "-t", window_id, option, value]);
    }
    run(tag)?;

    wait_for_prompt(tmux, pane_id);
    send_input_batch(tmux, pane_id, line.as_bytes(), InputDelivery::Paste)
        .map_err(anyhow::Error::msg)?;
    let mut enter = tmux()?;
    enter.args(["send-keys", "-t", pane_id, "Enter"]);
    run(enter)
}

/// The tagged workspace, or a new one. A new one also returns its first tab.
fn ensure_workspace(
    tmux: &Tmux,
    cwd: &str,
    embedded_codex: bool,
) -> anyhow::Result<(String, Option<(String, String)>)> {
    let mut list = tmux()?;
    list.args([
        "list-sessions",
        "-F",
        &format!("#{{session_id}}\t#{{{WORKSPACE_TAG}}}\t#{{session_name}}"),
    ]);
    // No server yet lists nothing; creating the session below starts it.
    let listed = list
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let mut names = Vec::new();
    for line in listed.lines() {
        let mut fields = line.splitn(3, '\t');
        let (Some(id), Some(tag), Some(name)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if tag == "1" {
            return Ok((id.to_owned(), None));
        }
        names.push(name.to_owned());
    }
    // A session someone else already called `automation` is theirs.
    let name = std::iter::once(WORKSPACE_NAME.to_owned())
        .chain((2..).map(|index| format!("{WORKSPACE_NAME}-{index}")))
        .find(|candidate| !names.contains(candidate))
        .expect("an unused name exists");
    let mut create = tmux()?;
    configure_new_session(
        &mut create,
        &v1::TmuxAction {
            name,
            directory: cwd.to_owned(),
            ..Default::default()
        },
        embedded_codex,
    )?;
    let ids = run_for_ids(create, &['$', '@', '%'])?;
    let mut tag = tmux()?;
    tag.args(["set-option", "-t", &ids[0], WORKSPACE_TAG, "1"]);
    run(tag)?;
    Ok((ids[0].clone(), Some((ids[1].clone(), ids[2].clone()))))
}

/// Waits, briefly, for the shell to draw anything: some shells drop typeahead
/// that arrives while they start. A shell that never draws gets the command
/// anyway.
fn wait_for_prompt(tmux: &Tmux, pane_id: &str) {
    for _ in 0..PROMPT_WAIT_STEPS {
        let Ok(mut capture) = tmux() else {
            return;
        };
        let drawn = capture
            .args(["capture-pane", "-p", "-t", pane_id])
            .output()
            .is_ok_and(|output| {
                output.status.success()
                    && !String::from_utf8_lossy(&output.stdout).trim().is_empty()
            });
        if drawn {
            return;
        }
        sleep(PROMPT_WAIT_STEP);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::plugins::automations::test_support::{TmuxFixture, definition};

    #[test]
    fn the_command_line_appends_the_prompt_file_as_one_argument() {
        let file = Path::new("/home/me/Library/Application Support/x/ci-3.txt");
        assert_eq!(
            command_line(&definition(Agent::Claude, ""), file).unwrap(),
            "claude --dangerously-skip-permissions -- \"$(cat '/home/me/Library/Application Support/x/ci-3.txt')\""
        );
        assert!(
            command_line(&definition(Agent::Codex, ""), file)
                .unwrap()
                .starts_with("codex --dangerously-bypass-approvals-and-sandbox -- \"$(cat '")
        );
        assert!(
            command_line(&definition(Agent::Custom, "cx --print"), file)
                .unwrap()
                .starts_with("cx --print \"$(cat '")
        );
        for unquotable in ["/tmp/it's/p.txt", "/tmp/back\\slash/p.txt"] {
            assert!(command_line(&definition(Agent::Claude, ""), Path::new(unquotable)).is_err());
        }
    }

    fn custom_in(dir: &Path, command: &str) -> Definition {
        Definition {
            cwd: dir.display().to_string(),
            ..definition(Agent::Custom, command)
        }
    }

    #[test]
    fn a_run_opens_a_tagged_tab_and_types_its_command() {
        let fixture = TmuxFixture::new();
        let dir = tempfile::tempdir().unwrap();
        let prompt = dir.path().join("ci-1.txt");
        std::fs::write(&prompt, "line one $HOME \"double\"\nline 'two'").unwrap();
        let out = dir.path().join("out.txt");
        let custom = custom_in(dir.path(), &format!("printf '%s' >'{}'", out.display()));

        let first = launch(&fixture.factory(), &custom, 1, &prompt, false).unwrap();
        let second = launch(&fixture.factory(), &custom, 2, &prompt, false).unwrap();
        assert_eq!(first.session_id, second.session_id, "one workspace");
        assert_ne!(first.window_id, second.window_id, "a tab per run");
        assert_eq!(
            fixture.display(&first.window_id, "#{session_name}"),
            "automation"
        );
        assert_eq!(
            fixture.display(&second.window_id, "#{window_name}"),
            "Fix CI #2"
        );
        assert_eq!(
            fixture.display(&second.window_id, "#{@muxflow-automation}"),
            "ci"
        );
        assert_eq!(
            fixture.display(&second.window_id, "#{@muxflow-automation-run}"),
            "2"
        );
        assert_eq!(
            fixture.display(&second.window_id, "#{@muxflow-name-locked}"),
            "1"
        );

        fixture.wait_until(|| std::fs::read_to_string(&out).is_ok_and(|text| !text.is_empty()));
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "line one $HOME \"double\"\nline 'two'"
        );

        // A program that sets its title cannot rename the tab.
        fixture.run(&["select-pane", "-t", &second.pane_id, "-T", "RENAMED"]);
        fixture.run(&[
            "send-keys",
            "-t",
            &second.pane_id,
            "printf '\\033kRENAMED\\033\\\\'",
            "Enter",
        ]);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            fixture.display(&second.window_id, "#{window_name}"),
            "Fix CI #2"
        );

        // Renaming the workspace does not make the next run create another.
        fixture.run(&["rename-session", "-t", &first.session_id, "mine"]);
        let third = launch(&fixture.factory(), &custom, 3, &prompt, false).unwrap();
        assert_eq!(third.session_id, first.session_id);
    }

    #[test]
    fn an_existing_session_named_automation_is_left_alone() {
        let fixture = TmuxFixture::new();
        fixture.run(&["new-session", "-d", "-s", "automation"]);
        let dir = tempfile::tempdir().unwrap();
        let prompt = dir.path().join("ci-1.txt");
        std::fs::write(&prompt, "hello").unwrap();
        let launched = launch(
            &fixture.factory(),
            &custom_in(dir.path(), "true"),
            1,
            &prompt,
            false,
        )
        .unwrap();
        assert_eq!(
            fixture.display(&launched.window_id, "#{session_name}"),
            "automation-2"
        );
    }

    #[test]
    fn a_missing_directory_fails_before_anything_opens() {
        let fixture = TmuxFixture::new();
        let missing = Definition {
            cwd: "/definitely/not/here".into(),
            ..definition(Agent::Custom, "true")
        };
        let error = launch(
            &fixture.factory(),
            &missing,
            1,
            Path::new("/tmp/p.txt"),
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("does not exist"), "{error:#}");
        assert!(!fixture.server_running(), "no workspace was created");
    }
}
