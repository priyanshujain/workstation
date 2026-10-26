use std::path::{Path, PathBuf};

use crate::conventions::Kind;
use crate::report::{CategorySnap, PathSnap, Report, project_snaps};
use crate::sweep::{self, Root, Rule};

pub fn refresh_kinds(
    report: &mut Report,
    kinds: &[String],
    project_roots: &[PathBuf],
    max_depth: usize,
) {
    let wanted = |name: &str| kinds.iter().any(|k| k.eq_ignore_ascii_case(name));
    let closed = crate::platform::closed_paths();

    let projects = if wanted(Kind::BuildOutput.label()) {
        let found = crate::projects::discover(project_roots, max_depth, &closed);
        report.projects = project_snaps(found.clone());
        found
    } else {
        report.to_projects()
    };
    let rules = crate::audit::attribution_rules(&closed, &projects);

    let targets = outermost(
        rules
            .iter()
            .filter(|r| wanted(&r.category))
            .map(|r| r.path.clone())
            .chain(
                report
                    .categories
                    .iter()
                    .filter(|c| wanted(&c.name))
                    .flat_map(|c| c.paths.iter().map(|p| p.path.clone())),
            )
            .collect(),
    );
    if !targets.is_empty() {
        remeasure(report, &targets, &rules, &closed);
    }

    if wanted(Kind::Nix.label()) {
        report.nix_garbage = crate::nix::garbage_bytes();
    }
}

fn remeasure(report: &mut Report, targets: &[PathBuf], rules: &[Rule], closed: &[PathBuf]) {
    let roots: Vec<Root> = targets
        .iter()
        .map(|path| Root {
            name: path.display().to_string(),
            path: path.clone(),
        })
        .collect();
    let swept = sweep::sweep_gated(&roots, rules, closed);
    let under_any = |path: &Path| targets.iter().any(|t| path.starts_with(t));

    for (target, measured) in targets.iter().zip(&swept.roots) {
        let cached: u64 = report
            .categories
            .iter()
            .flat_map(|c| c.paths.iter())
            .filter(|p| p.path.starts_with(target))
            .map(|p| p.size)
            .sum();
        shift_root(report, target, cached, measured.total);
    }

    for category in report.categories.iter_mut() {
        category.paths.retain(|p| !under_any(&p.path));
    }
    for (index, rule) in rules.iter().enumerate() {
        let size = swept.rule_sizes[index];
        if size == 0 || !under_any(&rule.path) {
            continue;
        }
        let entry = PathSnap {
            label: rule.label.clone(),
            path: rule.path.clone(),
            size,
            times: swept.rule_times[index],
        };
        match report
            .categories
            .iter_mut()
            .find(|c| c.name == rule.category)
        {
            Some(category) => category.paths.push(entry),
            None => report.categories.push(CategorySnap {
                name: rule.category.clone(),
                total_size: 0,
                paths: vec![entry],
            }),
        }
    }
    for category in report.categories.iter_mut() {
        category.total_size = category.paths.iter().map(|p| p.size).sum();
        category.paths.sort_by_key(|p| std::cmp::Reverse(p.size));
    }
    report.categories.retain(|c| c.total_size > 0);
    report
        .categories
        .sort_by_key(|c| std::cmp::Reverse(c.total_size));
}

fn shift_root(report: &mut Report, target: &Path, before: u64, after: u64) {
    let Some(root) = report
        .roots
        .iter_mut()
        .filter(|r| target.starts_with(&r.path))
        .max_by_key(|r| r.path.components().count())
    else {
        return;
    };
    let shift = |value: &mut u64| *value = (*value + after).saturating_sub(before);
    shift(&mut root.total);
    if let Some((_, size)) = root
        .children
        .iter_mut()
        .find(|(child, _)| target.starts_with(child))
    {
        shift(size);
    }
}

fn outermost(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths.dedup();
    let mut kept: Vec<PathBuf> = Vec::new();
    for path in paths {
        if !kept.iter().any(|k| path.starts_with(k)) {
            kept.push(path);
        }
    }
    kept
}
