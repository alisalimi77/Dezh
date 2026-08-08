//! # dezh-scheduler — Step 7: task placement spike
//!
//! Dezh's scheduler is not just "which thread gets the next CPU slice?". The
//! architectural target is a placement engine: pick the best resource for a
//! task based on policy, static task hints, runtime cost signals, and data
//! gravity. This crate validates that model without executing real work.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObjectKey(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    EfficiencyCpu,
    PerformanceCpu,
    Gpu,
    Npu,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Resource {
    pub node: NodeId,
    pub kind: ResourceKind,
    pub compute_score: f64,
    pub energy_cost: f64,
    pub queue_depth: u32,
    pub numa_distance: u32,
}

impl Resource {
    pub fn new(
        node: NodeId,
        kind: ResourceKind,
        compute_score: f64,
        energy_cost: f64,
        queue_depth: u32,
        numa_distance: u32,
    ) -> Self {
        Resource {
            node,
            kind,
            compute_score,
            energy_cost,
            queue_depth,
            numa_distance,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskClass {
    LatencySensitive,
    Batch,
    Interactive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkloadHint {
    Scalar,
    DataParallel,
    Tensor,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub class: TaskClass,
    pub hint: WorkloadHint,
    pub required_objects: Vec<ObjectKey>,
}

impl Task {
    pub fn new(class: TaskClass, hint: WorkloadHint, required_objects: Vec<ObjectKey>) -> Self {
        Task {
            class,
            hint,
            required_objects,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Mobile,
    Desktop,
    Server,
    LatencySensitive,
    Batch,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub resource: Resource,
    pub score: f64,
    pub explanation: PlacementExplanation,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlacementExplanation {
    pub compute: f64,
    pub accelerator_fit: f64,
    pub data_locality: f64,
    pub energy_penalty: f64,
    pub queue_penalty: f64,
    pub numa_penalty: f64,
    pub policy: Policy,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SchedulerError {
    NoResources,
    InvalidResource,
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SchedulerError::NoResources => write!(f, "no resources available"),
            SchedulerError::InvalidResource => {
                write!(
                    f,
                    "resource scores and costs must be finite positive numbers"
                )
            }
        }
    }
}

impl std::error::Error for SchedulerError {}

pub type Result<T> = std::result::Result<T, SchedulerError>;

#[derive(Default, Debug)]
pub struct ObjectLocations {
    map: HashMap<ObjectKey, NodeId>,
}

impl ObjectLocations {
    pub fn new() -> Self {
        ObjectLocations {
            map: HashMap::new(),
        }
    }

    pub fn set(&mut self, object: ObjectKey, node: NodeId) {
        self.map.insert(object, node);
    }

    pub fn get(&self, object: ObjectKey) -> Option<NodeId> {
        self.map.get(&object).copied()
    }
}

#[derive(Debug)]
pub struct PlacementEngine {
    resources: Vec<Resource>,
    locations: ObjectLocations,
}

impl PlacementEngine {
    pub fn new(resources: Vec<Resource>, locations: ObjectLocations) -> Result<Self> {
        if resources.is_empty() {
            return Err(SchedulerError::NoResources);
        }
        for r in &resources {
            if !r.compute_score.is_finite()
                || !r.energy_cost.is_finite()
                || r.compute_score <= 0.0
                || r.energy_cost <= 0.0
            {
                return Err(SchedulerError::InvalidResource);
            }
        }
        Ok(PlacementEngine {
            resources,
            locations,
        })
    }

    pub fn place(&self, task: &Task, policy: Policy) -> Placement {
        self.resources
            .iter()
            .map(|resource| self.score(task, policy, resource))
            .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(Ordering::Equal))
            .expect("resources validated non-empty")
    }

    pub fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn score(&self, task: &Task, policy: Policy, resource: &Resource) -> Placement {
        let compute = resource.compute_score * compute_weight(policy, task.class);
        let accelerator_fit =
            accelerator_fit(task.hint, resource.kind) * accelerator_weight(policy);
        let data_locality = self.data_locality(task, resource.node) * locality_weight(policy);
        let energy_penalty = resource.energy_cost * energy_weight(policy);
        let queue_penalty = f64::from(resource.queue_depth) * queue_weight(policy, task.class);
        let numa_penalty = f64::from(resource.numa_distance) * numa_weight(policy);
        let score = compute + accelerator_fit + data_locality
            - energy_penalty
            - queue_penalty
            - numa_penalty;
        Placement {
            resource: resource.clone(),
            score,
            explanation: PlacementExplanation {
                compute,
                accelerator_fit,
                data_locality,
                energy_penalty,
                queue_penalty,
                numa_penalty,
                policy,
            },
        }
    }

    fn data_locality(&self, task: &Task, node: NodeId) -> f64 {
        if task.required_objects.is_empty() {
            return 0.0;
        }
        let local = task
            .required_objects
            .iter()
            .filter(|object| self.locations.get(**object) == Some(node))
            .count();
        local as f64 / task.required_objects.len() as f64
    }
}

fn compute_weight(policy: Policy, class: TaskClass) -> f64 {
    match (policy, class) {
        (Policy::Mobile, _) => 0.7,
        (Policy::Desktop, TaskClass::Interactive) => 1.2,
        (Policy::Server, TaskClass::Batch) => 1.3,
        (Policy::LatencySensitive, _) => 1.0,
        (Policy::Batch, _) => 1.4,
        _ => 1.0,
    }
}

fn accelerator_weight(policy: Policy) -> f64 {
    match policy {
        Policy::Mobile => 3.0,
        Policy::Desktop => 2.0,
        Policy::Server => 2.2,
        Policy::LatencySensitive => 1.0,
        Policy::Batch => 2.4,
    }
}

fn accelerator_fit(hint: WorkloadHint, kind: ResourceKind) -> f64 {
    match (hint, kind) {
        (WorkloadHint::Tensor, ResourceKind::Npu) => 12.0,
        (WorkloadHint::Tensor, ResourceKind::Gpu) => 8.0,
        (WorkloadHint::DataParallel, ResourceKind::Gpu) => 10.0,
        (WorkloadHint::DataParallel, ResourceKind::Npu) => 5.0,
        (WorkloadHint::Scalar, ResourceKind::PerformanceCpu) => 5.0,
        (WorkloadHint::Scalar, ResourceKind::EfficiencyCpu) => 3.0,
        _ => 0.0,
    }
}

fn locality_weight(policy: Policy) -> f64 {
    match policy {
        Policy::Mobile => 4.0,
        Policy::Desktop => 5.0,
        Policy::Server => 12.0,
        Policy::LatencySensitive => 10.0,
        Policy::Batch => 8.0,
    }
}

fn energy_weight(policy: Policy) -> f64 {
    match policy {
        Policy::Mobile => 3.5,
        Policy::Desktop => 0.8,
        Policy::Server => 0.5,
        Policy::LatencySensitive => 0.3,
        Policy::Batch => 0.6,
    }
}

fn queue_weight(policy: Policy, class: TaskClass) -> f64 {
    match (policy, class) {
        (Policy::LatencySensitive, _) | (_, TaskClass::LatencySensitive) => 2.5,
        (Policy::Desktop, TaskClass::Interactive) => 1.8,
        (Policy::Batch, _) => 0.2,
        _ => 0.8,
    }
}

fn numa_weight(policy: Policy) -> f64 {
    match policy {
        Policy::Server => 1.6,
        Policy::LatencySensitive => 2.0,
        Policy::Batch => 0.7,
        Policy::Mobile | Policy::Desktop => 0.5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> PlacementEngine {
        let resources = vec![
            Resource::new(NodeId(0), ResourceKind::EfficiencyCpu, 3.0, 1.0, 0, 0),
            Resource::new(NodeId(1), ResourceKind::PerformanceCpu, 9.0, 4.0, 3, 1),
            Resource::new(NodeId(2), ResourceKind::Gpu, 14.0, 8.0, 1, 2),
            Resource::new(NodeId(3), ResourceKind::Npu, 8.0, 2.0, 0, 1),
        ];
        let mut locations = ObjectLocations::new();
        locations.set(ObjectKey(1), NodeId(1));
        locations.set(ObjectKey(2), NodeId(1));
        locations.set(ObjectKey(3), NodeId(2));
        PlacementEngine::new(resources, locations).unwrap()
    }

    #[test]
    fn mobile_tensor_prefers_low_energy_npu() {
        let task = Task::new(TaskClass::Interactive, WorkloadHint::Tensor, vec![]);
        let placement = engine().place(&task, Policy::Mobile);

        assert_eq!(placement.resource.kind, ResourceKind::Npu);
        assert!(placement.explanation.accelerator_fit > placement.explanation.energy_penalty);
    }

    #[test]
    fn server_batch_prefers_compute_near_data() {
        let task = Task::new(
            TaskClass::Batch,
            WorkloadHint::Scalar,
            vec![ObjectKey(1), ObjectKey(2)],
        );
        let placement = engine().place(&task, Policy::Server);

        assert_eq!(placement.resource.node, NodeId(1));
        assert!(placement.explanation.data_locality > 0.0);
    }

    #[test]
    fn latency_sensitive_avoids_busy_queue() {
        let resources = vec![
            Resource::new(NodeId(0), ResourceKind::PerformanceCpu, 12.0, 4.0, 50, 0),
            Resource::new(NodeId(1), ResourceKind::PerformanceCpu, 9.0, 4.0, 0, 0),
        ];
        let engine = PlacementEngine::new(resources, ObjectLocations::new()).unwrap();
        let task = Task::new(TaskClass::LatencySensitive, WorkloadHint::Scalar, vec![]);
        let placement = engine.place(&task, Policy::LatencySensitive);

        assert_eq!(placement.resource.node, NodeId(1));
        assert!(placement.explanation.queue_penalty < 1.0);
    }

    #[test]
    fn batch_policy_tolerates_queue_for_throughput() {
        let resources = vec![
            Resource::new(NodeId(0), ResourceKind::EfficiencyCpu, 3.0, 1.0, 0, 0),
            Resource::new(NodeId(1), ResourceKind::PerformanceCpu, 20.0, 5.0, 20, 0),
        ];
        let engine = PlacementEngine::new(resources, ObjectLocations::new()).unwrap();
        let task = Task::new(TaskClass::Batch, WorkloadHint::Scalar, vec![]);
        let placement = engine.place(&task, Policy::Batch);

        assert_eq!(placement.resource.node, NodeId(1));
    }

    #[test]
    fn invalid_resource_is_rejected() {
        let err = PlacementEngine::new(
            vec![Resource::new(
                NodeId(0),
                ResourceKind::PerformanceCpu,
                0.0,
                1.0,
                0,
                0,
            )],
            ObjectLocations::new(),
        )
        .unwrap_err();

        assert_eq!(err, SchedulerError::InvalidResource);
    }
}
