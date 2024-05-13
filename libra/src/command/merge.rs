use clap::Parser;
use venus::internal::object::commit::Commit;

use crate::{
    internal::{branch::Branch, head::Head},
    utils::util,
};

use super::{
    branch::get_target_commit,
    load_object, log,
    restore::{self, RestoreArgs},
};

#[derive(Parser, Debug)]
pub struct MergeArgs {
    /// The branch to merge into the current branch
    pub branch: String,
}

pub async fn execute(args: MergeArgs) {
    let target_commit_hash = get_target_commit(&args.branch).await;
    if target_commit_hash.is_err() {
        eprintln!("{}", target_commit_hash.err().unwrap());
        return;
    }
    let commit_hash = target_commit_hash.unwrap();

    let target_commit: Commit = load_object(&commit_hash).unwrap();
    let current_commit: Commit = load_object(&Head::current_commit().await.unwrap()).unwrap();
    let lca = lca_commit(&current_commit, &target_commit).await;

    if lca.is_none() {
        eprintln!("fatal: fatal: refusing to merge unrelated histories");
        return;
    }
    let lca = lca.unwrap();

    if lca.id == target_commit.id {
        // no need to merge
        println!("Already up to date.");
    } else if lca.id == current_commit.id {
        println!(
            "Updating {}..{}",
            &current_commit.id.to_plain_str()[..6],
            &target_commit.id.to_plain_str()[..6]
        );
        // fast-forward merge
        merge_ff(target_commit).await;
    } else {
        // didn't support yet
        eprintln!("fatal: Not possible to fast-forward merge, try merge manually");
    }
}

async fn lca_commit(lhs: &Commit, rhs: &Commit) -> Option<Commit> {
    let lhs_reachable = log::get_reachable_commits(lhs.id.to_plain_str()).await;
    let rhs_reachable = log::get_reachable_commits(rhs.id.to_plain_str()).await;

    // Commit `eq` is based on tree_id, so we shouldn't use it here

    for commit in lhs_reachable.iter() {
        if commit.id == rhs.id {
            return Some(commit.to_owned());
        }
    }

    for commit in rhs_reachable.iter() {
        if commit.id == lhs.id {
            return Some(commit.to_owned());
        }
    }

    for lhs_parrent in lhs_reachable.iter() {
        for rhs_parrent in rhs_reachable.iter() {
            if lhs_parrent.id == rhs_parrent.id {
                return Some(lhs_parrent.to_owned());
            }
        }
    }
    None
}

/// try merge in fast-forward mode, if it's not possible, do nothing
async fn merge_ff(commit: Commit) {
    println!("Fast-forward");
    // fast-forward merge
    let head = Head::current().await;
    match head {
        Head::Branch(branch_name) => {
            Branch::update_branch(&branch_name, &commit.id.to_plain_str(), None).await;
        }
        Head::Detached(_) => {
            Head::update(Head::Detached(commit.id), None).await;
        }
    }
    // change the working directory to the commit
    // restore all files to worktree from HEAD
    restore::execute(RestoreArgs {
        worktree: true,
        staged: true,
        source: None,
        pathspec: vec![util::working_dir_string()],
    })
    .await;
}
