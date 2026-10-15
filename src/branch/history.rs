//! 有界的拓扑历史：先子后父，保留所有父边（包括 merge）。
use super::BranchInfo;
use anyhow::Result;
use git2::{Oid, Repository, Sort};

pub(super) const LIMIT: usize = 200;

#[derive(Clone, Debug)]
pub(crate) struct CommitNode {
    pub oid: Oid,
    pub parents: Vec<Oid>,
    pub summary: String,
    pub author: String,
    pub lane: usize,
}

pub(super) fn load(
    repo: &Repository,
    branches: &[BranchInfo],
    head: Option<Oid>,
) -> Result<Vec<CommitNode>> {
    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
    for oid in branches.iter().map(|b| b.oid).chain(head) {
        walk.push(oid)?;
    }
    let mut lanes = Vec::<Option<Oid>>::new();
    let mut nodes = Vec::new();
    for oid in walk.take(LIMIT) {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        let lane = lanes
            .iter()
            .position(|slot| *slot == Some(oid))
            .unwrap_or_else(|| allocate(&mut lanes, oid));
        lanes[lane] = None;
        let parents: Vec<_> = commit.parent_ids().collect();
        for (index, parent) in parents.iter().enumerate() {
            if !lanes.contains(&Some(*parent)) {
                if index == 0 {
                    lanes[lane] = Some(*parent);
                } else {
                    allocate(&mut lanes, *parent);
                }
            }
        }
        nodes.push(CommitNode {
            oid,
            parents,
            lane,
            summary: commit.summary()?.unwrap_or("（无提交标题）").to_owned(),
            author: commit.author().name().unwrap_or("未知作者").to_owned(),
        });
    }
    Ok(nodes)
}

fn allocate(lanes: &mut Vec<Option<Oid>>, oid: Oid) -> usize {
    if let Some(index) = lanes.iter().position(Option::is_none) {
        lanes[index] = Some(oid);
        index
    } else {
        lanes.push(Some(oid));
        lanes.len() - 1
    }
}
