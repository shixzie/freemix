use std::collections::HashMap;
use std::hash::Hash;

use crate::{EntityRef, InputKind, Project, SourceRef, ValidationError, ValidationErrorKind};

// A project decoded from `project.json` can chain thousands of entities, so
// every search here is iterative and linear in the graph size: no recursion
// per reference link and no rescan of the entity lists per visited node.

pub(crate) fn mark_scene_cycles(project: &Project, errors: &mut Vec<ValidationError>) {
    let scenes = project.scenes();
    let scene_index = first_index(scenes.iter().map(|scene| scene.id));
    let inputs = project.inputs();
    let input_index = first_index(inputs.iter().map(|input| input.id));
    // A scene's edges come from the first scene carrying its id, which is the
    // scene any lookup by id resolves to.
    let edges = scenes
        .iter()
        .enumerate()
        .map(|(index, scene)| {
            if scene_index.get(&scene.id) != Some(&index) {
                return Vec::new();
            }
            scene
                .layers
                .iter()
                .filter_map(|layer| match layer.source {
                    SourceRef::Scene(next) => Some(next),
                    SourceRef::Input(input) => {
                        input_index
                            .get(&input)
                            .and_then(|&input| match inputs[input].kind {
                                InputKind::Scene { scene_id, .. } => Some(scene_id),
                                _ => None,
                            })
                    }
                })
                .filter_map(|next| scene_index.get(&next).copied())
                .collect()
        })
        .collect::<Vec<_>>();
    let on_cycle = nodes_on_cycles(&edges);
    for scene in scenes {
        if on_cycle[scene_index[&scene.id]] {
            errors.push(ValidationError {
                entity: Some(EntityRef::Scene(scene.id)),
                field: "layers.source",
                kind: ValidationErrorKind::Cycle,
            });
        }
    }
}

pub(crate) fn mark_audio_input_cycles(project: &Project, errors: &mut Vec<ValidationError>) {
    let inputs = project.inputs();
    let input_index = first_index(inputs.iter().map(|input| input.id));
    let edges = inputs
        .iter()
        .enumerate()
        .map(|(index, input)| {
            if input_index.get(&input.id) != Some(&index) {
                return Vec::new();
            }
            match input.kind {
                InputKind::Scene {
                    audio_source: Some(source),
                    ..
                } => input_index.get(&source).copied().into_iter().collect(),
                _ => Vec::new(),
            }
        })
        .collect::<Vec<_>>();
    let on_cycle = nodes_on_cycles(&edges);
    for input in inputs {
        if on_cycle[input_index[&input.id]] {
            errors.push(ValidationError {
                entity: Some(EntityRef::Input(input.id)),
                field: "kind.scene.audio_source",
                kind: ValidationErrorKind::Cycle,
            });
        }
    }
}

pub(crate) fn mark_bus_cycles(project: &Project, errors: &mut Vec<ValidationError>) {
    let buses = project.audio_buses();
    let bus_index = first_index(buses.iter().map(|bus| bus.id));
    let edges = buses
        .iter()
        .enumerate()
        .map(|(index, bus)| {
            if bus_index.get(&bus.id) != Some(&index) {
                return Vec::new();
            }
            bus.sends
                .iter()
                .filter_map(|send| bus_index.get(&send.destination).copied())
                .collect()
        })
        .collect::<Vec<_>>();
    let on_cycle = nodes_on_cycles(&edges);
    for bus in buses {
        if on_cycle[bus_index[&bus.id]] {
            errors.push(ValidationError {
                entity: Some(EntityRef::AudioBus(bus.id)),
                field: "sends.destination",
                kind: ValidationErrorKind::Cycle,
            });
        }
    }
}

/// Maps each id to the position of its first occurrence.
fn first_index<Id: Copy + Eq + Hash>(ids: impl Iterator<Item = Id>) -> HashMap<Id, usize> {
    let mut index = HashMap::new();
    for (position, id) in ids.enumerate() {
        index.entry(id).or_insert(position);
    }
    index
}

/// Marks every node that can reach itself: a member of a strongly connected
/// component with more than one node, or a node with an edge to itself.
///
/// Iterative Tarjan, so the cost is linear in nodes plus edges and the stack
/// use is constant regardless of how long a reference chain is.
fn nodes_on_cycles(edges: &[Vec<usize>]) -> Vec<bool> {
    let count = edges.len();
    let mut state = Tarjan {
        order: vec![None; count],
        low: vec![0; count],
        on_stack: vec![false; count],
        stack: Vec::new(),
        next_order: 0,
    };
    let mut on_cycle = vec![false; count];
    // Each frame is a node and the index of its next unexplored edge.
    let mut work: Vec<(usize, usize)> = Vec::new();

    for root in 0..count {
        if state.order[root].is_some() {
            continue;
        }
        state.visit(root);
        work.push((root, 0));
        while let Some(frame) = work.last_mut() {
            let (node, edge) = *frame;
            frame.1 += 1;
            if let Some(&next) = edges[node].get(edge) {
                if next == node {
                    on_cycle[node] = true;
                } else if let Some(next_order) = state.order[next] {
                    if state.on_stack[next] {
                        state.low[node] = state.low[node].min(next_order);
                    }
                } else {
                    state.visit(next);
                    work.push((next, 0));
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                state.low[parent] = state.low[parent].min(state.low[node]);
            }
            if Some(state.low[node]) == state.order[node] {
                let start = state
                    .stack
                    .iter()
                    .rposition(|&member| member == node)
                    .expect("an unfinished node is on the component stack");
                let component = state.stack.split_off(start);
                for &member in &component {
                    state.on_stack[member] = false;
                    if component.len() > 1 {
                        on_cycle[member] = true;
                    }
                }
            }
        }
    }
    on_cycle
}

struct Tarjan {
    order: Vec<Option<usize>>,
    low: Vec<usize>,
    on_stack: Vec<bool>,
    stack: Vec<usize>,
    next_order: usize,
}

impl Tarjan {
    fn visit(&mut self, node: usize) {
        self.order[node] = Some(self.next_order);
        self.low[node] = self.next_order;
        self.next_order += 1;
        self.stack.push(node);
        self.on_stack[node] = true;
    }
}
