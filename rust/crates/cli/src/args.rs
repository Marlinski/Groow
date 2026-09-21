//! The command line.
//!
//! One binary with two lives. Run by the owner it is the whole tool: it starts the core, opens
//! the window, and does the things only a mentor may do. Run by the mind, inside its own body,
//! the same binary is how it reaches its own alarms and thoughts, and the commands
//! that belong to its mentor refuse politely.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "groow", version, about = "A small mind that learns by changing its own weights")]
pub struct Cli {
    /// Where it lives. Everything it is is in there: its state, its skills, its commands, its
    /// manual and somewhere to work. Defaults to ./home in a checkout.
    #[arg(long, global = true)]
    pub home: Option<std::path::PathBuf>,

    /// The settings. Defaults to groow.json in its home, or the skeleton's until it has one.
    #[arg(long, global = true)]
    pub config: Option<std::path::PathBuf>,

    /// Print what the core replied, as it replied it, rather than as something to read.
    #[arg(long, global = true)]
    pub json: bool,

    /// What a new home is given: skills, manual, prompt script, settings. Only read, never
    /// written. Defaults to ./skel in a checkout, then /usr/share/groow/skel.
    #[arg(long, global = true)]
    pub skel: Option<std::path::PathBuf>,

    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Wake it up: run the core, here, until it is stopped.
    ///
    /// Where that "here" is — this terminal, a container, a microvm — is not this command's
    /// business. `make start` wakes it in the container this project ships with.
    Start {
        /// Run the mind as this user rather than as whoever started the core.
        #[arg(long)]
        as_user: Option<String>,
    },
    /// Put it to sleep: tell the core to stop. The body around it, if it has one and something
    /// keeps that body running, is not this command's business either.
    Stop,
    /// What it is doing just now.
    Status,
    /// Open the window onto it.
    Ui,
    /// Say something to it.
    Say {
        text: Vec<String>,
    },
    /// Read the conversation back.
    Recall {
        #[arg(short, long, default_value_t = 40)]
        n: usize,
    },
    /// Set an alarm for it.
    Remind {
        text: Vec<String>,
        /// A delay, such as 30m or 2h.
        #[arg(long = "in", name = "in")]
        in_: Option<String>,
        /// A time today or tomorrow, such as 18:30.
        #[arg(long)]
        at: Option<String>,
        /// A repeat, such as 2h or "daily 06:30".
        #[arg(long)]
        every: Option<String>,
    },
    /// Its alarms.
    /// Its alarms: `schedule`, or `schedule remove <id>`.
    Schedule {
        #[arg(default_value = "list")]
        action: String,
        id: Option<String>,
    },
    /// What it is thinking about on its own.
    Thoughts,
    /// Act on one of its thoughts.
    Thought {
        /// read, pause, resume or kill.
        action: String,
        id: String,
        text: Vec<String>,
    },
    /// Check the state on disk without needing the core.
    Doctor,

    /// Run one turn. The core starts these; you should not need to.
    #[command(hide = true)]
    RunTurn,
    /// Run one inner thought. The core starts these.
    #[command(hide = true)]
    RunThought { id: String },
}

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Command::Start { .. } => "start",
            Command::Stop => "stop",
            Command::Status => "status",
            Command::Ui => "ui",
            Command::Say { .. } => "say",
            Command::Recall { .. } => "recall",
            Command::Remind { .. } => "remind",
            Command::Schedule { .. } => "schedule",
            Command::Thoughts => "thoughts",
            Command::Thought { .. } => "thought",
            Command::Doctor => "doctor",
            Command::RunTurn => "run-turn",
            Command::RunThought { .. } => "run-thought",
        }
    }
}

/// Commands that belong to the mentor, and why the mind may not use them.
///
/// The point is not secrecy. Each of these would be a way for it to act on itself from the
/// outside, and the refusal says what it should do instead.
pub fn mentor_only(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        "start" => "you are already awake",
        "stop" => "you would only be putting yourself to sleep; finish the turn instead",
        "ui" => "there is no terminal here",
        _ => return None,
    })
}

/// Whether this process is the mind rather than a person at a keyboard.
pub fn is_the_mind() -> bool {
    std::env::var_os("GROOW_SELF").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn nonsense_is_refused_with_a_message_rather_than_a_panic() {
        // Skills are the mind's own files, not a thing the core has a command for.
        assert!(Cli::try_parse_from(["groow", "skill", "list"]).is_err());
        assert!(Cli::try_parse_from(["groow", "explode"]).is_err());
        assert!(Cli::try_parse_from(["groow"]).is_err(), "a bare command should say what it can do");
        assert!(Cli::try_parse_from(["groow", "thought", "read"]).is_err(), "a thought action needs an id");
    }

    #[test]
    fn the_mind_is_refused_the_commands_that_act_on_it_from_outside() {
        for c in ["start", "stop", "ui"] {
            let why = mentor_only(c).unwrap_or_else(|| panic!("{c} should belong to the mentor"));
            assert!(!why.is_empty(), "a refusal should say why");
        }
    }

    #[test]
    fn the_mind_keeps_the_commands_that_are_its_own_life() {
        for c in ["say", "inbox", "remind", "schedule", "thoughts", "thought", "recall", "doctor"] {
            assert_eq!(mentor_only(c), None, "{c} is how it lives; it must not be taken away");
        }
    }

    #[test]
    fn the_internal_commands_exist_but_are_not_advertised() {
        assert!(Cli::try_parse_from(["groow", "run-turn"]).is_ok());
        assert!(Cli::try_parse_from(["groow", "run-thought", "ab12"]).is_ok());
    }

    #[test]
    fn every_command_can_name_itself() {
        for args in [
            vec!["groow", "start"], vec!["groow", "stop"], vec!["groow", "status"],
            vec!["groow", "ui"], vec!["groow", "say", "x"], vec!["groow", "recall"],
vec!["groow", "remind", "x", "--in", "1h"],
            vec!["groow", "schedule"], vec!["groow", "thoughts"],
            vec!["groow", "thought", "read", "a1"], vec!["groow", "doctor"],
            vec!["groow", "run-turn"], vec!["groow", "run-thought", "a1"],
        ] {
            let c = Cli::try_parse_from(args.clone()).unwrap();
            assert!(!c.cmd.name().is_empty(), "{args:?}");
        }
    }
}
