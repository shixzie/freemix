use std::collections::HashMap;
use std::hash::Hash;

use fm_types::InputId;

use crate::{EntityRef, InputKind, Project, SourceRef, ValidationError, ValidationErrorKind};

// A project decoded from `project.json` can chain thousands of entities, so
// every search here is iterative and linear in the graph size: no recursion
// per reference link and no rescan of the entity lists per visited node.

pub(crate) fn mark_scene_cycles(project: &Project, errors: &mut Vec<ValidationError>) {
    let scenes = project.scenes();
    let scene_index = first_index(scenes.iter().map(|scene| scene.id));
    let inputs = project.inputs();
    let input_index = first_index(inputs.iter().map(|input| input.id));
    // A layer naming an alias depends on whatever the alias chain ends at.
    let terminals = alias_terminals(project, &input_index);
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
                    SourceRef::Input(input) => input_index
                        .get(&input)
                        .and_then(|&input| terminals[input])
                        .and_then(|input| match inputs[input].kind {
                            InputKind::Scene { scene_id, .. } => Some(scene_id),
                            // Leaves end the walk; a mix feed is the explicit
                            // one-frame delay node, so it never closes a cycle.
                            InputKind::Color
                            | InputKind::Media { .. }
                            | InputKind::Device { .. }
                            | InputKind::NetworkStream(_)
                            | InputKind::SystemAudio(_)
                            | InputKind::Simulated(_)
                            | InputKind::MixFeed(_)
                            | InputKind::Title(_)
                            | InputKind::Alias { .. } => None,
                        }),
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
            // An alias hears its target, so it carries the audio dependency.
            match input.kind {
                InputKind::Scene {
                    audio_source: Some(source),
                    ..
                }
                | InputKind::Alias { target: source, .. } => {
                    input_index.get(&source).copied().into_iter().collect()
                }
                InputKind::Scene {
                    audio_source: None, ..
                }
                | InputKind::Color
                | InputKind::Media { .. }
                | InputKind::Device { .. }
                | InputKind::NetworkStream(_)
                | InputKind::SystemAudio(_)
                | InputKind::Simulated(_)
                | InputKind::MixFeed(_)
                | InputKind::Title(_) => Vec::new(),
            }
        })
        .collect::<Vec<_>>();
    let on_cycle = nodes_on_cycles(&edges);
    // Every audio cycle passes through a scene's audio source; a cycle made
    // only of aliases is reported once, by `mark_alias_cycles`.
    for input in inputs {
        let has_audio_source = matches!(
            input.kind,
            InputKind::Scene {
                audio_source: Some(_),
                ..
            }
        );
        if has_audio_source && on_cycle[input_index[&input.id]] {
            errors.push(ValidationError {
                entity: Some(EntityRef::Input(input.id)),
                field: "kind.scene.audio_source",
                kind: ValidationErrorKind::Cycle,
            });
        }
    }
}

pub(crate) fn mark_alias_cycles(project: &Project, errors: &mut Vec<ValidationError>) {
    let inputs = project.inputs();
    let input_index = first_index(inputs.iter().map(|input| input.id));
    let edges = inputs
        .iter()
        .enumerate()
        .map(|(index, input)| {
            if input_index.get(&input.id) != Some(&index) {
                return Vec::new();
            }
            alias_target(&input.kind)
                .and_then(|target| input_index.get(&target).copied())
                .into_iter()
                .collect()
        })
        .collect::<Vec<_>>();
    let on_cycle = nodes_on_cycles(&edges);
    for input in inputs {
        if on_cycle[input_index[&input.id]] {
            errors.push(ValidationError {
                entity: Some(EntityRef::Input(input.id)),
                field: "kind.alias.target",
                kind: ValidationErrorKind::Cycle,
            });
        }
    }
}

fn alias_target(kind: &InputKind) -> Option<InputId> {
    match kind {
        InputKind::Alias { target, .. } => Some(*target),
        InputKind::Color
        | InputKind::Media { .. }
        | InputKind::Device { .. }
        | InputKind::NetworkStream(_)
        | InputKind::SystemAudio(_)
        | InputKind::Scene { .. }
        | InputKind::Simulated(_)
        | InputKind::MixFeed(_)
        | InputKind::Title(_) => None,
    }
}

/// For each input position, the position of the non-alias input its alias
/// chain ends at (itself when it is not an alias), or `None` when the chain
/// reaches a missing input or an alias cycle.
///
/// Iterative and memoized, so the total cost is linear in the input count.
fn alias_terminals(project: &Project, input_index: &HashMap<InputId, usize>) -> Vec<Option<usize>> {
    let inputs = project.inputs();
    let mut terminal: Vec<Option<Option<usize>>> = vec![None; inputs.len()];
    let mut on_path = vec![false; inputs.len()];
    let mut path = Vec::new();
    for root in 0..inputs.len() {
        let mut current = Some(root);
        let result = loop {
            let Some(node) = current else {
                break None;
            };
            if let Some(known) = terminal[node] {
                break known;
            }
            if on_path[node] {
                break None;
            }
            match alias_target(&inputs[node].kind) {
                Some(target) => {
                    on_path[node] = true;
                    path.push(node);
                    current = input_index.get(&target).copied();
                }
                None => break Some(node),
            }
        };
        if path.is_empty() && terminal[root].is_none() {
            terminal[root] = Some(result);
        }
        for node in path.drain(..) {
            on_path[node] = false;
            terminal[node] = Some(result);
        }
    }
    terminal
        .into_iter()
        .map(|known| known.expect("every input position was resolved"))
        .collect()
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
