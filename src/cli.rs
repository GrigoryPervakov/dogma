//! Command-line interface (clap).

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "dogma", version, about = "Terminal UI for Nerve")]
pub struct Args {
    /// Nerve server URL, as `url` or `name=url`. Repeat to drive several
    /// instances at once (their resources merge into one UI, tagged per
    /// instance). Default: http://127.0.0.1:8900.
    #[arg(long, env = "NERVE_URL")]
    pub server: Vec<String>,

    /// Open this session id directly on launch.
    #[arg(long)]
    pub session: Option<String>,

    /// Disable colored output (forced black-and-white).
    #[arg(long)]
    pub no_color: bool,

    /// Verbose logging (debug level). File lives at ~/.dogma/dogma.log,
    /// override path with the DOGMA_LOG environment variable.
    #[arg(short, long)]
    pub verbose: bool,
}
