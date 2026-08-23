//! Turning pairwise "these two are the same" decisions into duplicate groups.
//!
//! Matching produces edges between files. Grouping is the transitive closure of
//! those edges, which is exactly what a union-find structure computes.

use crate::models::FileId;

/// Disjoint-set union with path compression and union by rank.
#[derive(Debug)]
pub struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<u8>,
}

impl UnionFind {
    pub fn new(len: usize) -> Self {
        Self {
            parent: (0..len).collect(),
            rank: vec![0; len],
        }
    }

    pub fn find(&mut self, mut node: usize) -> usize {
        while self.parent[node] != node {
            // Path halving: keeps the tree flat without a second pass.
            self.parent[node] = self.parent[self.parent[node]];
            node = self.parent[node];
        }
        node
    }

    /// Returns true when the two nodes were not already joined.
    pub fn union(&mut self, a: usize, b: usize) -> bool {
        let (mut ra, mut rb) = (self.find(a), self.find(b));
        if ra == rb {
            return false;
        }
        if self.rank[ra] < self.rank[rb] {
            std::mem::swap(&mut ra, &mut rb);
        }
        self.parent[rb] = ra;
        if self.rank[ra] == self.rank[rb] {
            self.rank[ra] += 1;
        }
        true
    }

    /// All sets with more than one member, each sorted, in a deterministic order.
    pub fn groups(&mut self) -> Vec<Vec<FileId>> {
        let mut buckets: std::collections::HashMap<usize, Vec<FileId>> =
            std::collections::HashMap::new();
        for node in 0..self.parent.len() {
            let root = self.find(node);
            buckets.entry(root).or_default().push(node);
        }
        let mut groups: Vec<Vec<FileId>> = buckets
            .into_values()
            .filter(|members| members.len() > 1)
            .map(|mut members| {
                members.sort_unstable();
                members
            })
            .collect();
        groups.sort_unstable();
        groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitive_edges_form_one_group() {
        let mut uf = UnionFind::new(5);
        uf.union(0, 1);
        uf.union(1, 2);
        uf.union(3, 4);
        assert_eq!(uf.groups(), vec![vec![0, 1, 2], vec![3, 4]]);
    }

    #[test]
    fn unconnected_files_form_no_group() {
        let mut uf = UnionFind::new(3);
        assert!(uf.groups().is_empty());
    }
}
