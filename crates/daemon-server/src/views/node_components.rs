//! Immutable node rows. Key-derived priorities make the ordered tree independent of edits.
use super::observation::{Metric, Observation, Serialization};
use alondra_view_model::LiveNode;
use serde::{Serialize, Serializer, ser::SerializeSeq};
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock};

type Hash = [u8; 32];
type Link = Option<Arc<Branch>>;

#[derive(Debug)]
struct Leaf {
    row: LiveNode,
    priority: Hash,
    digest: OnceLock<Hash>,
}

impl Leaf {
    fn new(row: LiveNode, priority: Option<Hash>, observation: &Observation) -> Arc<Self> {
        let priority = priority.unwrap_or_else(|| hash_parts(b"alondra/node-priority\0", &[row.agent_id.as_bytes()], observation));
        Arc::new(Self { row, priority, digest: OnceLock::new() })
    }

    fn digest(&self, observation: &Observation, kind: Serialization) -> Hash {
        *self.digest.get_or_init(|| {
            observation.add(Metric::NodeLeafSerializations, 1);
            let bytes = observation.serialize_component("nodes", &self.row, kind);
            hash_parts(b"alondra/node-row\0", &[&bytes], observation)
        })
    }

    fn precedes(&self, other: &Self) -> bool {
        (&self.priority, &self.row.agent_id) < (&other.priority, &other.row.agent_id)
    }
}

#[derive(Debug)]
struct Branch {
    leaf: Arc<Leaf>,
    left: Link,
    right: Link,
    len: usize,
    digest: OnceLock<Hash>,
}

impl Branch {
    fn new(leaf: Arc<Leaf>, left: Link, right: Link) -> Arc<Self> {
        Arc::new(Self { len: 1 + len(&left) + len(&right), leaf, left, right, digest: OnceLock::new() })
    }

    fn copy(&self, leaf: Arc<Leaf>, left: Link, right: Link, observation: &Observation) -> Arc<Self> {
        observation.add(Metric::NodeTreeCopies, 1);
        Self::new(leaf, left, right)
    }

    fn digest(&self, observation: &Observation, kind: Serialization) -> Hash {
        *self.digest.get_or_init(|| {
            let left = tree_digest(&self.left, observation, kind);
            let row = self.leaf.digest(observation, kind);
            let right = tree_digest(&self.right, observation, kind);
            hash_parts(b"alondra/node-branch\0", &[
                &(len(&self.left) as u64).to_be_bytes(), &left, &row,
                &(len(&self.right) as u64).to_be_bytes(), &right,
            ], observation)
        })
    }
}

fn len(tree: &Link) -> usize { tree.as_ref().map_or(0, |node| node.len) }
fn shared(left: &Link, right: &Link) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
        (None, None) => true,
        _ => false,
    }
}

fn hash_parts(domain: &[u8], parts: &[&[u8]], observation: &Observation) -> Hash {
    let mut hash = Sha256::new();
    hash.update(domain);
    let mut bytes = domain.len() as u64;
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part);
        bytes += 8 + part.len() as u64;
    }
    observation.add(Metric::HashOperations, 1);
    observation.add(Metric::HashBytes, bytes);
    hash.finalize().into()
}

fn tree_digest(tree: &Link, observation: &Observation, kind: Serialization) -> Hash {
    // Empty children have a reserved representation, not a hash operation done on every path.
    tree.as_ref().map_or([0; 32], |node| node.digest(observation, kind))
}

fn split(tree: &Link, key: &str, observation: &Observation) -> (Link, Link) {
    let Some(node) = tree else { return (None, None) };
    observation.add(Metric::NodeTreeVisits, 1);
    if node.leaf.row.agent_id.as_str() < key {
        let (left, right) = split(&node.right, key, observation);
        (Some(node.copy(node.leaf.clone(), node.left.clone(), left, observation)), right)
    } else {
        let (left, right) = split(&node.left, key, observation);
        (left, Some(node.copy(node.leaf.clone(), right, node.right.clone(), observation)))
    }
}

fn insert(tree: &Link, leaf: Arc<Leaf>, observation: &Observation) -> Arc<Branch> {
    let Some(node) = tree else { return Branch::new(leaf, None, None) };
    observation.add(Metric::NodeTreeVisits, 1);
    if leaf.row.agent_id == node.leaf.row.agent_id {
        return node.copy(leaf, node.left.clone(), node.right.clone(), observation);
    }
    if leaf.precedes(&node.leaf) {
        let (left, right) = split(tree, &leaf.row.agent_id, observation);
        return Branch::new(leaf, left, right);
    }
    if leaf.row.agent_id < node.leaf.row.agent_id {
        node.copy(node.leaf.clone(), Some(insert(&node.left, leaf, observation)), node.right.clone(), observation)
    } else {
        node.copy(node.leaf.clone(), node.left.clone(), Some(insert(&node.right, leaf, observation)), observation)
    }
}

fn merge(left: &Link, right: &Link, observation: &Observation) -> Link {
    let (Some(a), Some(b)) = (left, right) else { return left.clone().or_else(|| right.clone()) };
    observation.add(Metric::NodeTreeVisits, 1);
    if a.leaf.precedes(&b.leaf) {
        Some(a.copy(a.leaf.clone(), a.left.clone(), merge(&a.right, right, observation), observation))
    } else {
        Some(b.copy(b.leaf.clone(), merge(left, &b.left, observation), b.right.clone(), observation))
    }
}

fn remove(tree: &Link, key: &str, observation: &Observation) -> Link {
    let node = tree.as_ref()?;
    observation.add(Metric::NodeTreeVisits, 1);
    match key.cmp(node.leaf.row.agent_id.as_str()) {
        std::cmp::Ordering::Equal => merge(&node.left, &node.right, observation),
        std::cmp::Ordering::Less => {
            let left = remove(&node.left, key, observation);
            if shared(&left, &node.left) { return tree.clone(); }
            Some(node.copy(node.leaf.clone(), left, node.right.clone(), observation))
        }
        std::cmp::Ordering::Greater => {
            let right = remove(&node.right, key, observation);
            if shared(&right, &node.right) { return tree.clone(); }
            Some(node.copy(node.leaf.clone(), node.left.clone(), right, observation))
        }
    }
}

fn find<'a>(tree: &'a Link, key: &str, observation: Option<&Observation>) -> Option<&'a Arc<Leaf>> {
    let mut tree = tree.as_ref();
    while let Some(node) = tree {
        if let Some(observation) = observation { observation.add(Metric::NodeTreeVisits, 1); }
        match key.cmp(node.leaf.row.agent_id.as_str()) {
            std::cmp::Ordering::Equal => return Some(&node.leaf),
            std::cmp::Ordering::Less => tree = node.left.as_ref(),
            std::cmp::Ordering::Greater => tree = node.right.as_ref(),
        }
    }
    None
}

fn equal_leaf(left: &Arc<Leaf>, right: &Arc<Leaf>, observation: &Observation) -> bool {
    if Arc::ptr_eq(left, right) { return true; }
    observation.add(Metric::NodeLeafComparisons, 1);
    observation.add(Metric::ComponentRowsCompared, 1);
    left.row == right.row
}

fn equal_tree(left: &Link, right: &Link, observation: &Observation) -> bool {
    if shared(left, right) { return true; }
    let (Some(left), Some(right)) = (left, right) else { return false };
    observation.add(Metric::NodeTreeVisits, 1);
    left.len == right.len && equal_leaf(&left.leaf, &right.leaf, observation)
        && equal_tree(&left.left, &right.left, observation)
        && equal_tree(&left.right, &right.right, observation)
}

/// Reuse a rebuilt tree even if insertion or removal changed its root. Lookups in the previous
/// tree find every unchanged leaf; matching subtrees also retain their already computed hashes.
fn reuse_tree(tree: &Link, previous: &Link, observation: &Observation) -> Link {
    reuse_tree_at(tree, previous, previous, observation)
}

fn reuse_tree_at(tree: &Link, previous: &Link, all_previous: &Link, observation: &Observation) -> Link {
    let Some(node) = tree else { return None };
    if shared(tree, previous) { return tree.clone(); }
    observation.add(Metric::NodeTreeVisits, 1);
    let matching = previous.as_ref().filter(|old| old.leaf.row.agent_id == node.leaf.row.agent_id)
        .or_else(|| matching_branch(all_previous, &node.leaf.row.agent_id, observation));
    if matching.is_some_and(|old| Arc::ptr_eq(old, node)) { return tree.clone(); }
    let leaf = matching.filter(|old| equal_leaf(&node.leaf, &old.leaf, observation))
        .map_or_else(|| node.leaf.clone(), |old| old.leaf.clone());
    let left = reuse_tree_at(&node.left, matching.map_or(&None, |old| &old.left), all_previous, observation);
    let right = reuse_tree_at(&node.right, matching.map_or(&None, |old| &old.right), all_previous, observation);
    if let Some(old) = matching {
        if Arc::ptr_eq(&leaf, &old.leaf) && shared(&left, &old.left) && shared(&right, &old.right) {
            return Some(old.clone());
        }
    }
    if Arc::ptr_eq(&leaf, &node.leaf) && shared(&left, &node.left) && shared(&right, &node.right) {
        return tree.clone();
    }
    Some(node.copy(leaf, left, right, observation))
}

fn matching_branch<'a>(tree: &'a Link, key: &str, observation: &Observation) -> Option<&'a Arc<Branch>> {
    let mut tree = tree.as_ref();
    while let Some(node) = tree {
        observation.add(Metric::NodeTreeVisits, 1);
        match key.cmp(node.leaf.row.agent_id.as_str()) {
            std::cmp::Ordering::Equal => return Some(node),
            std::cmp::Ordering::Less => tree = node.left.as_ref(),
            std::cmp::Ordering::Greater => tree = node.right.as_ref(),
        }
    }
    None
}

#[derive(Clone, Debug)]
enum Rows {
    Ordered(Link),
    // Arbitrary public fixtures may have duplicate IDs or a different order. Preserve them;
    // keyed mutation is deliberately unavailable rather than silently changing their JSON.
    Sequence { leaves: Arc<Vec<Arc<Leaf>>>, digest: Arc<OnceLock<Hash>> },
}

#[derive(Clone, Debug)]
pub(super) struct Nodes(Rows);

impl Nodes {
    pub(super) fn new(rows: Vec<LiveNode>, observation: &Observation) -> Self {
        let ordered = rows.windows(2).all(|pair| {
            observation.add(Metric::NodeTreeVisits, 1);
            pair[0].agent_id < pair[1].agent_id
        });
        if ordered {
            let mut tree = None;
            for row in rows { tree = Some(insert(&tree, Leaf::new(row, None, observation), observation)); }
            Self(Rows::Ordered(tree))
        } else {
            Self(Rows::Sequence {
                leaves: Arc::new(rows.into_iter().map(|row| Leaf::new(row, None, observation)).collect()),
                digest: Arc::default(),
            })
        }
    }

    pub(super) fn value(&self) -> &Self { self }
    pub(super) fn len(&self) -> usize {
        match &self.0 { Rows::Ordered(tree) => len(tree), Rows::Sequence { leaves, .. } => leaves.len() }
    }
    pub(super) fn get(&self, id: &str) -> Option<&LiveNode> {
        match &self.0 {
            Rows::Ordered(tree) => find(tree, id, None).map(|leaf| &leaf.row),
            Rows::Sequence { leaves, .. } => leaves.iter().find(|leaf| leaf.row.agent_id == id).map(|leaf| &leaf.row),
        }
    }

    pub(super) fn get_observed(&self, id: &str, observation: &Observation) -> Option<&LiveNode> {
        match &self.0 {
            Rows::Ordered(tree) => find(tree, id, Some(observation)).map(|leaf| &leaf.row),
            Rows::Sequence { leaves, .. } => leaves.iter().find(|leaf| {
                observation.add(Metric::NodeTreeVisits, 1);
                leaf.row.agent_id == id
            }).map(|leaf| &leaf.row),
        }
    }

    /// False means the caller must use its full projection: IDs are ambiguous or the candidate
    /// does not describe the requested node. A missing candidate removes just that key.
    pub(super) fn update(&mut self, id: &str, row: Option<LiveNode>, observation: &Observation) -> bool {
        if row.as_ref().is_some_and(|row| row.agent_id != id) { return false; }
        let Rows::Ordered(tree) = &mut self.0 else { return false };
        if let Some(row) = row {
            let old = find(tree, id, Some(observation));
            if let Some(old) = old {
                observation.add(Metric::NodeLeafComparisons, 1);
                observation.add(Metric::ComponentRowsCompared, 1);
                if old.row == row { return true; }
            }
            let leaf = Leaf::new(row, old.map(|old| old.priority), observation);
            *tree = Some(insert(tree, leaf, observation));
        } else {
            *tree = remove(tree, id, observation);
        }
        true
    }

    pub(super) fn iter(&self) -> Iter<'_> {
        let mut iter = Iter { stack: Vec::new(), sequence: None, remaining: self.len() };
        match &self.0 {
            Rows::Ordered(tree) => iter.descend(tree.as_deref()),
            Rows::Sequence { leaves, .. } => iter.sequence = Some(leaves.iter()),
        }
        iter
    }

    pub(super) fn materialize(&self, observation: &Observation) -> Vec<LiveNode> {
        observation.add(Metric::NodeMaterializedRows, self.len() as u64);
        observation.add(Metric::MaterializedNodeRows, self.len() as u64);
        self.iter().cloned().collect()
    }

    pub(super) fn same(&self, other: &Self, observation: &Observation) -> bool {
        match (&self.0, &other.0) {
            (Rows::Ordered(left), Rows::Ordered(right)) => equal_tree(left, right, observation),
            (Rows::Sequence { leaves: left, .. }, Rows::Sequence { leaves: right, .. }) =>
                Arc::ptr_eq(left, right) || (left.len() == right.len() && left.iter().zip(right.iter()).all(|(a, b)| equal_leaf(a, b, observation))),
            _ => false,
        }
    }

    pub(super) fn reuse(&mut self, previous: &Self, observation: &Observation) {
        match (&mut self.0, &previous.0) {
            (Rows::Ordered(tree), Rows::Ordered(old)) => *tree = reuse_tree(tree, old, observation),
            (Rows::Sequence { leaves, digest }, Rows::Sequence { leaves: old, digest: old_digest }) => {
                if Arc::ptr_eq(leaves, old) { return; }
                let mut all = leaves.len() == old.len();
                let reused = leaves.iter().enumerate().map(|(index, leaf)| {
                    if let Some(previous) = old.get(index).filter(|previous| equal_leaf(leaf, previous, observation)) {
                        previous.clone()
                    } else { all = false; leaf.clone() }
                }).collect();
                if all { *leaves = old.clone(); *digest = old_digest.clone(); }
                else { *leaves = Arc::new(reused); }
            }
            _ => {},
        }
    }

    pub(super) fn digest(&self, observation: &Observation, kind: Serialization) -> Hash {
        match &self.0 {
            Rows::Ordered(tree) => tree_digest(tree, observation, kind),
            Rows::Sequence { leaves, digest } => *digest.get_or_init(|| {
                let hashes: Vec<_> = leaves.iter().map(|leaf| leaf.digest(observation, kind)).collect();
                let mut parts: Vec<&[u8]> = Vec::with_capacity(hashes.len() + 1);
                let count = (leaves.len() as u64).to_be_bytes();
                parts.push(&count);
                parts.extend(hashes.iter().map(|hash| hash.as_slice()));
                hash_parts(b"alondra/node-sequence\0", &parts, observation)
            }),
        }
    }
}

pub(super) struct Iter<'a> {
    stack: Vec<&'a Branch>,
    sequence: Option<std::slice::Iter<'a, Arc<Leaf>>>,
    remaining: usize,
}
impl<'a> Iter<'a> {
    fn descend(&mut self, mut tree: Option<&'a Branch>) {
        while let Some(node) = tree { self.stack.push(node); tree = node.left.as_deref(); }
    }
}
impl<'a> Iterator for Iter<'a> {
    type Item = &'a LiveNode;
    fn next(&mut self) -> Option<Self::Item> {
        let leaf = if let Some(sequence) = &mut self.sequence { &sequence.next()?.row }
        else { let node = self.stack.pop()?; self.descend(node.right.as_deref()); &node.leaf.row };
        self.remaining -= 1;
        Some(leaf)
    }
    fn size_hint(&self) -> (usize, Option<usize>) { (self.remaining, Some(self.remaining)) }
}
impl ExactSizeIterator for Iter<'_> {}

impl Serialize for Nodes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.len()))?;
        for row in self.iter() { sequence.serialize_element(row)?; }
        sequence.end()
    }
}

#[cfg(test)]
#[cfg(not(feature = "real-hosts"))]
mod tests {
    use super::*;

    fn row(id: &str) -> LiveNode {
        LiveNode { agent_id: id.into(), summary: Some("A visible historical node".repeat(8)), ..Default::default() }
    }
    fn rows(count: usize) -> Vec<LiveNode> { (0..count).map(|id| row(&format!("node-{id:05}"))).collect() }
    fn observed() -> Observation { let observation = Observation::armed(); observation.start(); observation }
    fn difference(observation: &Observation, before: &super::super::observation::Totals, metric: Metric) -> u64 {
        observation.totals().metrics[metric as usize] - before.metrics[metric as usize]
    }

    #[test]
    fn unchanged_nodes_do_not_visit_copy_serialize_or_hash_rows() {
        let observation = observed();
        for count in [32, 4096] {
            let nodes = Nodes::new(rows(count), &observation);
            let digest = nodes.digest(&observation, Serialization::Digest);
            let before = observation.totals();
            let clone = nodes.clone();
            assert!(nodes.same(&clone, &observation));
            assert_eq!(clone.digest(&observation, Serialization::Sampler), digest);
            assert_eq!(observation.totals(), before);
        }
    }

    #[test]
    fn one_changed_row_only_serializes_that_leaf_and_hashes_its_tree_path() {
        let observation = observed();
        for count in [32, 4096] {
            let original = rows(count);
            let mut nodes = Nodes::new(original.clone(), &observation);
            let before_digest = nodes.digest(&observation, Serialization::Digest);
            let retained = nodes.clone();
            let mut candidate = original[count / 2].clone();
            candidate.summary = Some("The one changed row".into());
            let key = candidate.agent_id.clone();
            let payload_bytes = serde_json::to_vec(&candidate).unwrap().len() as u64;
            let before = observation.totals();
            assert!(nodes.update(&key, Some(candidate.clone()), &observation));
            assert_ne!(nodes.digest(&observation, Serialization::Digest), before_digest);
            assert_eq!(difference(&observation, &before, Metric::NodeLeafSerializations), 1);
            assert_eq!(difference(&observation, &before, Metric::DigestNodesBytes), payload_bytes);
            assert_eq!(difference(&observation, &before, Metric::NodeLeafComparisons), 1);
            let copies = difference(&observation, &before, Metric::NodeTreeCopies);
            assert!((1..=64).contains(&copies), "only the search path is copied: {copies}");
            assert_eq!(difference(&observation, &before, Metric::HashOperations), copies + 1);
            assert!(difference(&observation, &before, Metric::NodeTreeVisits) <= 128);
            assert_eq!(difference(&observation, &before, Metric::MaterializedNodeRows), 0);
            assert_eq!(retained.get(&key), Some(&original[count / 2]));
            assert_eq!(retained.digest(&observation, Serialization::Digest), before_digest);
            let mut reference = original;
            reference[count / 2] = candidate;
            assert_eq!(serde_json::to_vec(&nodes).unwrap(), serde_json::to_vec(&reference).unwrap());
        }
    }

    #[test]
    fn equal_candidate_and_absent_removal_keep_the_warm_tree() {
        let observation = observed();
        let mut nodes = Nodes::new(rows(32), &observation);
        let original = nodes.clone();
        let digest = nodes.digest(&observation, Serialization::Digest);
        let candidate = nodes.get("node-00003").unwrap().clone();
        let before = observation.totals();
        assert!(nodes.update("node-00003", Some(candidate), &observation));
        assert!(nodes.update("absent", None, &observation));
        assert!(nodes.same(&original, &observation));
        assert_eq!(nodes.digest(&observation, Serialization::Digest), digest);
        for metric in [Metric::NodeTreeCopies, Metric::NodeLeafSerializations, Metric::HashOperations, Metric::HashBytes] {
            assert_eq!(difference(&observation, &before, metric), 0);
        }
    }

    #[test]
    fn keyed_edits_and_full_rebuild_have_identical_order_bytes_and_digest() {
        let observation = observed();
        let reference = rows(64);
        let mut edited = Nodes::new(Vec::new(), &observation);
        for row in reference.iter().rev() {
            assert!(edited.update(&row.agent_id, Some(row.clone()), &observation));
        }
        let rebuilt = Nodes::new(reference.clone(), &observation);
        assert_eq!(edited.digest(&observation, Serialization::Digest), rebuilt.digest(&observation, Serialization::Digest));
        for id in [0, 1, 31, 63] {
            assert!(edited.update(&reference[id].agent_id, None, &observation));
        }
        let without: Vec<_> = reference.iter().enumerate().filter(|(index, _)| ![0, 1, 31, 63].contains(index))
            .map(|(_, row)| row.clone()).collect();
        assert_eq!(serde_json::to_vec(&edited).unwrap(), serde_json::to_vec(&without).unwrap());
        assert_eq!(edited.digest(&observation, Serialization::Digest), Nodes::new(without, &observation).digest(&observation, Serialization::Digest));
        for id in [31, 0, 63, 1] {
            assert!(edited.update(&reference[id].agent_id, Some(reference[id].clone()), &observation));
        }
        assert_eq!(serde_json::to_vec(&edited).unwrap(), serde_json::to_vec(&reference).unwrap());
        assert_eq!(edited.digest(&observation, Serialization::Digest), rebuilt.digest(&observation, Serialization::Digest));
        assert_eq!(edited.materialize(&observation), reference);
    }

    #[test]
    fn full_projection_reuses_unchanged_leaf_and_subtree_hashes() {
        let observation = observed();
        for count in [32, 4096] {
            let reference = rows(count);
            let previous = Nodes::new(reference.clone(), &observation);
            previous.digest(&observation, Serialization::Digest);
            let mut changed = reference;
            changed[count / 2].working = true;
            let mut next = Nodes::new(changed, &observation);
            next.reuse(&previous, &observation);
            let before = observation.totals();
            next.digest(&observation, Serialization::Digest);
            assert_eq!(difference(&observation, &before, Metric::NodeLeafSerializations), 1);
            assert!(difference(&observation, &before, Metric::HashOperations) <= 65);
        }
    }

    #[test]
    fn reusing_an_incrementally_changed_tree_does_not_revisit_unchanged_subtrees() {
        let observation = observed();
        let previous = Nodes::new(rows(4096), &observation);
        previous.digest(&observation, Serialization::Digest);
        let mut next = previous.clone();
        let mut row = next.get("node-02048").unwrap().clone();
        row.working = true;
        assert!(next.update("node-02048", Some(row), &observation));
        let before = observation.totals();
        next.reuse(&previous, &observation);
        assert!(difference(&observation, &before, Metric::NodeTreeVisits) < 64);
        assert_eq!(difference(&observation, &before, Metric::NodeTreeCopies), 0);
        assert_eq!(difference(&observation, &before, Metric::NodeLeafComparisons), 1);
    }

    #[test]
    fn full_projection_reuses_leaves_after_insertion_and_removal_change_tree_shape() {
        let observation = observed();
        let mut reference = rows(64);
        let previous = Nodes::new(reference.clone(), &observation);
        previous.digest(&observation, Serialization::Digest);
        reference.remove(12);
        reference.push(row("z-new"));
        let mut next = Nodes::new(reference.clone(), &observation);
        next.reuse(&previous, &observation);
        let before = observation.totals();
        next.digest(&observation, Serialization::Digest);
        assert_eq!(difference(&observation, &before, Metric::NodeLeafSerializations), 1);
        assert_eq!(serde_json::to_vec(&next).unwrap(), serde_json::to_vec(&reference).unwrap());
    }

    #[test]
    fn arbitrary_order_and_duplicate_ids_preserve_the_wire_without_keyed_mutation() {
        let observation = observed();
        for reference in [vec![row("z"), row("a")], vec![row("a"), row("a")]] {
            let mut nodes = Nodes::new(reference.clone(), &observation);
            let digest = nodes.digest(&observation, Serialization::Digest);
            assert!(!nodes.update("a", None, &observation));
            assert!(!nodes.update("a", Some(row("a")), &observation));
            assert_eq!(serde_json::to_vec(&nodes).unwrap(), serde_json::to_vec(&reference).unwrap());
            assert_eq!(nodes.materialize(&observation), reference);
            let mut fresh = Nodes::new(reference, &observation);
            fresh.reuse(&nodes, &observation);
            let before = observation.totals();
            assert_eq!(fresh.digest(&observation, Serialization::Digest), digest);
            assert!(fresh.same(&nodes, &observation));
            assert_eq!(observation.totals(), before);
        }
        let mut nodes = Nodes::new(rows(2), &observation);
        assert!(!nodes.update("node-00000", Some(row("wrong")), &observation));
        assert_eq!(nodes.materialize(&observation), rows(2));
    }
}
