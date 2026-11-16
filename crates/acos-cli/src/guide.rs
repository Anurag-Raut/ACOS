//! `acos guide [topic]` — the agent guide from the CLI.
//!
//! One source of truth: the text is [`acos_mcp::help::guide`], the exact
//! function the MCP `acos_help` tool serves (it slices the embedded
//! `docs/AGENT_GUIDE.md`). Before this command the guide was reachable only
//! through MCP, so an agent told "use the CLI only" had to read the checkout's
//! markdown instead (root cause F: two front doors with different behaviour).
//!
//! There is deliberately no second copy of the guide and no CLI-specific
//! variant: a topic here returns the same bytes `acos_help {"topic": ...}`
//! returns, and `acos guide all` the same full text. The generated `mcp` topic
//! is built from the advertised tool descriptors by the same function.

use std::process::ExitCode;

use acos_mcp::help;

/// Arguments for `acos guide`.
#[derive(Debug, clap::Args)]
pub struct GuideArgs {
    /// Topic to print (`workflow`, `cli`, `scenarios`, `errors`, `limits`,
    /// `mcp`, or `all`); default is the bounded overview.
    #[arg(value_name = "TOPIC")]
    pub topic: Option<String>,

    /// Topic to print, in the `acos_help {"topic": ...}` spelling.
    #[arg(long = "topic", value_name = "TOPIC")]
    pub topic_flag: Option<String>,
}

impl GuideArgs {
    fn resolved_topic(&self) -> Option<&str> {
        self.topic_flag
            .as_deref()
            .or(self.topic.as_deref())
            .filter(|topic| !topic.is_empty())
    }
}

/// Run `acos guide [topic]`.
pub fn run(args: GuideArgs) -> ExitCode {
    let topic = args.resolved_topic();
    // The guide is the command's data, so it goes to stdout like any other
    // command result. A wrong topic is a human diagnostic on stderr (the same
    // split `--json` commands use), never a silent substitution with no trace.
    if let Some(topic) = topic {
        if !help::is_topic(topic) {
            eprintln!(
                "acos guide: unknown topic `{topic}`; printing the overview. Topics: {}",
                help::topic_list().join(", ")
            );
        }
    }
    println!("{}", help::guide(topic));
    ExitCode::SUCCESS
}
