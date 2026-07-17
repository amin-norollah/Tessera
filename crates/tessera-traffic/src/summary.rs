//! Trace summary tool (P3.5-F8): message count, size mix, dependency depth,
//! and per-source injection profile for a [`NetraceTrace`], computed once
//! before running so a study can sanity-check a trace without a full
//! simulation.

use std::collections::{HashMap, VecDeque};

use crate::netrace::{NetraceTrace, SizeClass};

#[derive(Clone, Debug)]
pub struct TraceSummary {
    pub message_count: usize,
    pub control_count: usize,
    pub data_count: usize,
    /// Longest chain of dependency edges (in messages) anywhere in the
    /// trace: an approximation of the workload's critical-path length.
    pub max_dependency_depth: u32,
    /// Messages injected per trace-native source node id.
    pub per_node_injection: HashMap<u32, usize>,
}

impl TraceSummary {
    pub fn report(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(s, "messages: {}", self.message_count);
        let _ = writeln!(
            s,
            "size mix: {} control, {} data",
            self.control_count, self.data_count
        );
        let _ = writeln!(s, "max dependency depth: {}", self.max_dependency_depth);
        let mut nodes: Vec<_> = self.per_node_injection.keys().copied().collect();
        nodes.sort_unstable();
        let _ = writeln!(s, "per-node injection profile:");
        for node in nodes {
            let _ = writeln!(s, "  node {node}: {}", self.per_node_injection[&node]);
        }
        s
    }
}

/// Summarizes `trace`. `trace` is assumed acyclic (guaranteed by
/// [`NetraceTrace::from_reader`]/[`NetraceTrace::select_region`], the only
/// ways to construct one), so depth computation is a single stack-safe
/// topological pass, not recursion.
pub fn summarize(trace: &NetraceTrace) -> TraceSummary {
    let n = trace.messages.len();
    let ids: HashMap<u64, usize> = trace
        .messages
        .iter()
        .enumerate()
        .map(|(i, m)| (m.id, i))
        .collect();

    let mut indegree = vec![0u32; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, m) in trace.messages.iter().enumerate() {
        indegree[i] = m.deps.len() as u32;
        for d in &m.deps {
            dependents[ids[d]].push(i);
        }
    }

    let mut depth = vec![1u32; n];
    let mut remaining = indegree.clone();
    let mut queue: VecDeque<usize> = indegree
        .iter()
        .enumerate()
        .filter(|&(_, &d)| d == 0)
        .map(|(i, _)| i)
        .collect();
    while let Some(i) = queue.pop_front() {
        for &dep in &dependents[i] {
            depth[dep] = depth[dep].max(depth[i] + 1);
            remaining[dep] -= 1;
            if remaining[dep] == 0 {
                queue.push_back(dep);
            }
        }
    }
    let max_dependency_depth = depth.iter().copied().max().unwrap_or(0);

    let mut control_count = 0;
    let mut data_count = 0;
    let mut per_node_injection: HashMap<u32, usize> = HashMap::new();
    for m in &trace.messages {
        match m.size_class {
            SizeClass::Control => control_count += 1,
            SizeClass::Data => data_count += 1,
        }
        *per_node_injection.entry(m.src).or_insert(0) += 1;
    }

    TraceSummary {
        message_count: n,
        control_count,
        data_count,
        max_dependency_depth,
        per_node_injection,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_message_count_size_mix_and_injection_profile() {
        let trace = NetraceTrace::parse(
            "version 1\n\
             0 0 3 control -\n\
             1 0 3 data 0\n\
             2 1 3 control -\n",
        )
        .unwrap();
        let summary = summarize(&trace);
        assert_eq!(summary.message_count, 3);
        assert_eq!(summary.control_count, 2);
        assert_eq!(summary.data_count, 1);
        assert_eq!(summary.per_node_injection[&0], 2);
        assert_eq!(summary.per_node_injection[&1], 1);
    }

    #[test]
    fn dependency_depth_follows_the_longest_chain() {
        // 0 -> 1 -> 2 -> 3 is a 4-deep chain; 4 is an independent single node.
        let trace = NetraceTrace::parse(
            "version 1\n\
             0 0 1 control -\n\
             1 0 1 control 0\n\
             2 0 1 control 1\n\
             3 0 1 control 2\n\
             4 0 1 control -\n",
        )
        .unwrap();
        let summary = summarize(&trace);
        assert_eq!(summary.max_dependency_depth, 4);
    }

    #[test]
    fn report_renders_every_section() {
        let trace = NetraceTrace::parse("version 1\n0 0 1 control -\n").unwrap();
        let report = summarize(&trace).report();
        assert!(report.contains("messages: 1"));
        assert!(report.contains("node 0: 1"));
    }
}
