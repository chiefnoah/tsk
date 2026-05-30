use crate::errors::Result;
use crate::namespace;
use crate::object::{self, StableId, Task};
use crate::properties;
use crate::task::{self, ParsedLink};
use crate::workspace::Id;
use git2::Repository;
use std::collections::{BTreeMap, BTreeSet};

pub type BindingMap = BTreeMap<StableId, Vec<(String, Id)>>;

pub struct Refresh {
    pub old_refs: BTreeSet<String>,
    pub new_refs: BTreeSet<String>,
    pub source_ref: String,
}

pub fn set_calculated_property(
    attrs: &mut BTreeMap<String, Vec<String>>,
    key: &str,
    values: BTreeSet<String>,
) {
    if values.is_empty() {
        attrs.remove(key);
    } else {
        attrs.insert(key.to_string(), values.into_iter().collect());
    }
}

pub fn split_human_ref(value: &str) -> Option<(String, Id)> {
    let value = value
        .strip_prefix("[[")
        .and_then(|v| v.strip_suffix("]]"))
        .unwrap_or(value);
    let (namespace, id) = value.rsplit_once('-')?;
    namespace::validate_name(namespace).ok()?;
    let id = id.parse::<u32>().ok()?;
    Some((namespace.to_string(), Id(id)))
}

pub fn human_ref(namespace: &str, id: Id) -> String {
    format!("[[{namespace}-{}]]", id.0)
}

pub fn resolve_human_ref(repo: &Repository, value: &str) -> Result<Option<StableId>> {
    let Some((namespace, id)) = split_human_ref(value) else {
        return Ok(None);
    };
    namespace::lookup(repo, &namespace, id.0)
}

pub fn linked_refs(active_namespace: &str, content: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Some(parsed) = task::parse(content) else {
        return out;
    };
    for link in parsed.links {
        match link {
            ParsedLink::Internal(id) => {
                out.insert(human_ref(active_namespace, id));
            }
            ParsedLink::Foreign { prefix, id } => {
                out.insert(human_ref(&prefix, Id(id)));
            }
            ParsedLink::Namespaced { namespace, id } => {
                out.insert(human_ref(&namespace, id));
            }
            ParsedLink::External(_) => {}
        }
    }
    out
}

pub fn refresh_task(
    repo: &Repository,
    active_namespace: &str,
    task: &mut Task,
    source_id: Id,
    source_stable: &StableId,
) -> Result<Refresh> {
    let source_ref = human_ref(active_namespace, source_id);
    let old_refs: BTreeSet<String> = task
        .properties
        .get(properties::REFERENCES_KEY)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let new_refs = linked_refs(active_namespace, &task.content);
    set_calculated_property(
        &mut task.properties,
        properties::REFERENCES_KEY,
        new_refs.clone(),
    );

    let resolves_to_source = |r: &String| {
        resolve_human_ref(repo, r)
            .ok()
            .flatten()
            .is_some_and(|s| &s == source_stable)
    };
    let self_linked = new_refs.iter().any(resolves_to_source);
    let self_was_linked = old_refs.iter().any(resolves_to_source);
    if self_linked || self_was_linked {
        let mut backlinks: BTreeSet<String> = task
            .properties
            .get(properties::REFERENCED_BY_KEY)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();
        if self_linked {
            backlinks.insert(source_ref.clone());
        } else {
            backlinks.remove(&source_ref);
        }
        set_calculated_property(
            &mut task.properties,
            properties::REFERENCED_BY_KEY,
            backlinks,
        );
    }

    Ok(Refresh {
        old_refs,
        new_refs,
        source_ref,
    })
}

pub fn sync_referenced_by(
    repo: &Repository,
    source_stable: &StableId,
    source_ref: &str,
    old_refs: &BTreeSet<String>,
    new_refs: &BTreeSet<String>,
) -> Result<()> {
    let mut touched = old_refs.clone();
    touched.extend(new_refs.iter().cloned());

    for target_ref in touched {
        let Some(target_stable) = resolve_human_ref(repo, &target_ref)? else {
            continue;
        };
        if &target_stable == source_stable {
            continue;
        }
        let Some(mut target) = object::read(repo, &target_stable)? else {
            continue;
        };
        let mut backlinks: BTreeSet<String> = target
            .properties
            .get(properties::REFERENCED_BY_KEY)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();
        if new_refs.contains(&target_ref) {
            backlinks.insert(source_ref.to_string());
        } else {
            backlinks.remove(source_ref);
        }
        set_calculated_property(
            &mut target.properties,
            properties::REFERENCED_BY_KEY,
            backlinks,
        );
        properties::update_task(repo, &target_stable, &target, "update-backlinks")?;
    }
    Ok(())
}

pub fn binding_map<F>(repo: &Repository, task_exists: F) -> Result<BindingMap>
where
    F: Fn(&StableId) -> bool,
{
    let mut bindings: BindingMap = BTreeMap::new();
    for namespace_name in namespace::list_names(repo)? {
        let ns = namespace::read(repo, &namespace_name)?;
        for (human, stable) in ns.mapping {
            if task_exists(&stable) {
                bindings
                    .entry(stable)
                    .or_default()
                    .push((namespace_name.clone(), Id(human)));
            }
        }
    }
    for values in bindings.values_mut() {
        values.sort();
    }
    Ok(bindings)
}

pub fn canonical_binding(
    bindings: &BindingMap,
    active_namespace: &str,
    stable: &StableId,
) -> Option<(String, Id)> {
    let bindings = bindings.get(stable)?;
    bindings
        .iter()
        .find(|(namespace, _)| namespace == active_namespace)
        .or_else(|| bindings.first())
        .cloned()
}

pub fn rewrite_renumbered_properties(
    attrs: &mut BTreeMap<String, Vec<String>>,
    renumbers: &[(u32, u32)],
) -> bool {
    let original = attrs.clone();
    for key in [properties::REFERENCES_KEY, properties::REFERENCED_BY_KEY] {
        let Some(values) = attrs.get_mut(key) else {
            continue;
        };
        for value in &mut *values {
            for (old, new) in renumbers {
                if value == &format!("tsk-{old}") {
                    *value = format!("tsk-{new}");
                } else if value == &format!("[[tsk-{old}]]") {
                    *value = format!("[[tsk-{new}]]");
                }
            }
        }
        values.sort();
        values.dedup();
    }
    attrs != &original
}
