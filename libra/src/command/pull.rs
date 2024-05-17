use crate::internal::{config::Config, head::Head};

use super::{fetch, merge};
use clap::Parser;
#[derive(Parser, Debug)]
pub struct PullArgs;

pub async fn execute(args: PullArgs) {
    let _ = args;
    let fetch_args = fetch::FetchArgs::parse_from(Vec::<String>::new());
    fetch::execute(fetch_args).await;

    let head = Head::current().await;
    match head {
        Head::Branch(name) => match Config::branch_config(&name).await {
            Some(branch_config) => {
                let merge_args = merge::MergeArgs {
                    branch: format!("{}/{}", branch_config.remote, branch_config.merge),
                };
                merge::execute(merge_args).await;
            }
            None => {
                eprintln!("There is no tracking information for the current branch.");
                eprintln!("hint: set up a tracking branch with `libra branch --set-upstream-to=<remote>/<branch>`")
            }
        },
        _ => {
            eprintln!("You are not currently on a branch.");
        }
    }
}
