//! Cycle detection over the marimo dependency graph: iterative Tarjan SCC
//! with all per-node state kept in the DFS work-stack frames, so no map
//! lookup can miss and no panic path exists.

use std::collections::{HashMap, HashSet};

/// One DFS frame; `index`/`lowlink` live here while the node is on the work
/// stack, mirroring the recursive formulation.
struct Frame {
    node: usize,
    /// Position of the next parent-edge neighbor to visit.
    pos: usize,
    index: usize,
    lowlink: usize,
}

/// Strongly connected components with more than one member (= dependency
/// cycles), over the `parents` adjacency. Deterministic: roots are visited in
/// sorted order and components are sorted.
pub(super) fn cycles(parents: &HashMap<usize, Vec<usize>>) -> Vec<Vec<usize>> {
    let mut index_of: HashMap<usize, usize> = HashMap::new();
    let mut on_stack: HashSet<usize> = HashSet::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut counter = 0usize;
    let mut sccs: Vec<Vec<usize>> = Vec::new();

    // Map keys are unique, so a sorted Vec states the visit order directly.
    let mut roots: Vec<usize> = parents.keys().copied().collect();
    roots.sort_unstable();
    for root in roots {
        if index_of.contains_key(&root) {
            continue;
        }
        let mut work: Vec<Frame> = Vec::new();
        index_of.insert(root, counter);
        stack.push(root);
        on_stack.insert(root);
        work.push(Frame {
            node: root,
            pos: 0,
            index: counter,
            lowlink: counter,
        });
        counter += 1;

        while let Some(frame) = work.last_mut() {
            let neighbors = parents.get(&frame.node).map(Vec::as_slice).unwrap_or(&[]);
            if let Some(&neighbor) = neighbors.get(frame.pos) {
                frame.pos += 1;
                if let Some(&existing) = index_of.get(&neighbor) {
                    if on_stack.contains(&neighbor) {
                        frame.lowlink = frame.lowlink.min(existing);
                    }
                } else {
                    index_of.insert(neighbor, counter);
                    stack.push(neighbor);
                    on_stack.insert(neighbor);
                    work.push(Frame {
                        node: neighbor,
                        pos: 0,
                        index: counter,
                        lowlink: counter,
                    });
                    counter += 1;
                }
            } else {
                let (node, lowlink, index) = (frame.node, frame.lowlink, frame.index);
                work.pop();
                // lowlink == index marks an SCC root.
                if lowlink == index {
                    let mut component = Vec::new();
                    while let Some(member) = stack.pop() {
                        on_stack.remove(&member);
                        component.push(member);
                        if member == node {
                            break;
                        }
                    }
                    if component.len() > 1 {
                        component.sort_unstable();
                        sccs.push(component);
                    }
                }
                if let Some(parent) = work.last_mut() {
                    parent.lowlink = parent.lowlink.min(lowlink);
                }
            }
        }
    }
    sccs.sort();
    sccs
}
