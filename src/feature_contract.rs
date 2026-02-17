use anyhow::Context;
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;

const ALLOWED_CLI_TOOLS: &[&str] = &["claude_code", "codex", "opencode"];

#[derive(Debug, Clone, Deserialize)]
pub struct FeatureContract {
    pub feature_id: String,
    #[serde(default)]
    pub lane: Option<String>,
    #[serde(default)]
    pub risk: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub tasks: Vec<FeatureTask>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeatureTask {
    pub id: String,
    pub goal: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub ghost: Option<String>,
    #[serde(default)]
    pub lane: Option<String>,
    #[serde(default)]
    pub risk: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub auto_store: Option<String>,
    #[serde(default)]
    pub wait_secs: Option<u64>,
    #[serde(default)]
    pub cli_tool: Option<String>,
    #[serde(default)]
    pub cli_model: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

pub fn load_feature_contract(path: &Path) -> anyhow::Result<FeatureContract> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Failed to read feature contract file {}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let mut parsed: FeatureContract = match ext {
        "json" => serde_json::from_str(&raw).context("Failed to parse JSON feature contract"),
        "yml" | "yaml" => {
            serde_yaml::from_str(&raw).context("Failed to parse YAML feature contract")
        }
        _ => serde_yaml::from_str(&raw)
            .or_else(|_| serde_json::from_str(&raw))
            .context("Failed to parse feature contract as YAML or JSON"),
    }?;
    parsed.validate()?;
    Ok(parsed)
}

impl FeatureContract {
    pub fn validate(&mut self) -> anyhow::Result<()> {
        let mut errors: Vec<String> = Vec::new();
        if self.feature_id.trim().is_empty() {
            errors.push("feature_id must not be empty".to_string());
        }
        normalize_lane_and_risk(
            self.lane.as_deref(),
            self.risk.as_deref(),
            "contract defaults",
            &mut errors,
        );
        if self.tasks.is_empty() {
            errors.push("tasks must not be empty".to_string());
        }

        let mut ids = std::collections::HashSet::new();
        for task in &self.tasks {
            let label = format!("task '{}'", task.id);
            if task.id.trim().is_empty() {
                errors.push("task id must not be empty".to_string());
            } else if !ids.insert(task.id.clone()) {
                errors.push(format!("duplicate task id '{}'", task.id));
            }
            if task.enabled && task.goal.trim().is_empty() {
                errors.push(format!("{} has empty goal", label));
            }
            normalize_lane_and_risk(
                task.lane.as_deref(),
                task.risk.as_deref(),
                &label,
                &mut errors,
            );
            if let Some(tool) = task.cli_tool.as_deref() {
                if !ALLOWED_CLI_TOOLS.contains(&tool) {
                    errors.push(format!(
                        "{} has unsupported cli_tool '{}' (expected one of: {})",
                        label,
                        tool,
                        ALLOWED_CLI_TOOLS.join(", ")
                    ));
                }
            }
            for dep in &task.depends_on {
                if dep == &task.id {
                    errors.push(format!("{} depends on itself", label));
                }
            }
        }

        let task_by_id: HashMap<String, &FeatureTask> =
            self.tasks.iter().map(|t| (t.id.clone(), t)).collect();
        for task in &self.tasks {
            if !task.enabled {
                continue;
            }
            for dep in &task.depends_on {
                match task_by_id.get(dep) {
                    None => errors.push(format!(
                        "task '{}' depends on missing task '{}'",
                        task.id, dep
                    )),
                    Some(dep_task) if !dep_task.enabled => errors.push(format!(
                        "task '{}' depends on disabled task '{}'",
                        task.id, dep
                    )),
                    _ => {}
                }
            }
        }

        if errors.is_empty() {
            if let Err(e) = self.execution_batches() {
                errors.push(e.to_string());
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(
                "Invalid feature contract ({}):\n- {}",
                self.feature_id,
                errors.join("\n- ")
            );
        }
    }

    pub fn task_by_id(&self, id: &str) -> Option<&FeatureTask> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn execution_order(&self) -> anyhow::Result<Vec<String>> {
        let batches = self.execution_batches()?;
        Ok(batches.into_iter().flatten().collect())
    }

    pub fn execution_batches(&self) -> anyhow::Result<Vec<Vec<String>>> {
        let enabled: Vec<&FeatureTask> = self.tasks.iter().filter(|t| t.enabled).collect();
        if enabled.is_empty() {
            return Ok(Vec::new());
        }

        let mut indegree: HashMap<String, usize> = HashMap::new();
        let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
        for task in &enabled {
            indegree.insert(task.id.clone(), 0);
            adjacency.entry(task.id.clone()).or_default();
        }

        for task in &enabled {
            for dep in &task.depends_on {
                if indegree.contains_key(dep) {
                    *indegree
                        .get_mut(&task.id)
                        .expect("enabled node must have indegree") += 1;
                    adjacency
                        .entry(dep.clone())
                        .or_default()
                        .push(task.id.clone());
                }
            }
        }

        let mut ready: BTreeSet<String> = indegree
            .iter()
            .filter(|(_, degree)| **degree == 0)
            .map(|(id, _)| id.clone())
            .collect();
        let mut processed = 0usize;
        let mut batches: Vec<Vec<String>> = Vec::new();

        while !ready.is_empty() {
            let current: Vec<String> = ready.iter().cloned().collect();
            ready.clear();
            for node in &current {
                processed += 1;
                if let Some(children) = adjacency.get(node) {
                    for child in children {
                        if let Some(degree) = indegree.get_mut(child) {
                            *degree = degree.saturating_sub(1);
                            if *degree == 0 {
                                ready.insert(child.clone());
                            }
                        }
                    }
                }
            }
            batches.push(current);
        }

        if processed != enabled.len() {
            anyhow::bail!(
                "task dependency graph contains a cycle among enabled tasks (processed {} of {})",
                processed,
                enabled.len()
            );
        }

        Ok(batches)
    }
}

fn normalize_lane_and_risk(
    lane: Option<&str>,
    risk: Option<&str>,
    label: &str,
    errors: &mut Vec<String>,
) {
    if let Some(l) = lane {
        if !matches!(l, "delivery" | "self_improvement") {
            errors.push(format!(
                "{} has invalid lane '{}' (expected delivery | self_improvement)",
                label, l
            ));
        }
    }
    if let Some(r) = risk {
        if !matches!(r, "low" | "medium" | "high") {
            errors.push(format!(
                "{} has invalid risk '{}' (expected low | medium | high)",
                label, r
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_yaml(yaml: &str) -> FeatureContract {
        let mut c: FeatureContract = serde_yaml::from_str(yaml).unwrap();
        c.validate().unwrap();
        c
    }

    #[test]
    fn dag_batches_are_stable_and_layered() {
        let c = parse_yaml(
            r#"
feature_id: demo
lane: delivery
tasks:
  - id: T1
    goal: one
  - id: T2
    goal: two
    depends_on: [T1]
  - id: T3
    goal: three
    depends_on: [T1]
  - id: T4
    goal: four
    depends_on: [T2, T3]
"#,
        );
        assert_eq!(
            c.execution_batches().unwrap(),
            vec![
                vec!["T1".to_string()],
                vec!["T2".to_string(), "T3".to_string()],
                vec!["T4".to_string()],
            ]
        );
        assert_eq!(
            c.execution_order().unwrap(),
            vec![
                "T1".to_string(),
                "T2".to_string(),
                "T3".to_string(),
                "T4".to_string(),
            ]
        );
    }

    #[test]
    fn detects_cycle() {
        let mut c: FeatureContract = serde_yaml::from_str(
            r#"
feature_id: cyc
tasks:
  - id: A
    goal: a
    depends_on: [B]
  - id: B
    goal: b
    depends_on: [A]
"#,
        )
        .unwrap();
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("contains a cycle"));
    }

    #[test]
    fn rejects_dependency_on_disabled_task() {
        let mut c: FeatureContract = serde_yaml::from_str(
            r#"
feature_id: dep
tasks:
  - id: A
    goal: a
    enabled: false
  - id: B
    goal: b
    depends_on: [A]
"#,
        )
        .unwrap();
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("depends on disabled task"));
    }
}
