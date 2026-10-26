use std::path::PathBuf;

use crate::conventions::{self, Places};
use crate::projects::Project;
use crate::sweep::{self, Denial, Denials, Root, RootUsage, Rule, Unreadable};

pub struct Category {
    pub name: String,
    pub paths: Vec<CategoryPath>,
    pub total_size: u64,
}

pub struct CategoryPath {
    pub label: String,
    pub path: PathBuf,
    pub size: u64,
}

/// One pass over the volume, read two ways: by root, which always adds up to
/// the volume, and by category, which is a naming layer over the same bytes.
pub struct Audit {
    pub roots: Vec<RootUsage>,
    pub categories: Vec<Category>,
}

impl Audit {
    /// Everything the walk could actually see.
    pub fn measured(&self) -> u64 {
        self.roots.iter().map(|r| r.total).sum()
    }

    pub fn attributed(&self) -> u64 {
        self.categories.iter().map(|c| c.total_size).sum()
    }

    /// Measured but unnamed. This is the number the old audit threw away.
    pub fn unattributed(&self) -> u64 {
        self.roots.iter().map(|r| r.unattributed_total).sum()
    }

    pub fn unreadable_count(&self) -> usize {
        self.denials().total()
    }

    pub fn denials(&self) -> Denials {
        self.roots
            .iter()
            .fold(Denials::default(), |mut total, root| {
                total.merge(root.denials);
                total
            })
    }

    /// Sample of what could not be opened, so "grant Full Disk Access" is
    /// advice the user can act on rather than a shrug.
    pub fn unreadable_examples(&self, denial: Denial, limit: usize) -> Vec<&Unreadable> {
        self.roots
            .iter()
            .flat_map(|r| r.unreadable.iter())
            .filter(|u| u.denial == denial)
            .take(limit)
            .collect()
    }

    /// How many directories refused for this reason. Full Disk Access fixes
    /// one of the two and not the other.
    pub fn denied(&self, denial: Denial) -> usize {
        self.denials().of(denial)
    }

    /// Biggest unnamed directories anywhere, so the gap is actionable rather
    /// than just honest.
    pub fn largest_unattributed(&self, limit: usize) -> Vec<(&PathBuf, u64)> {
        let mut all: Vec<(&PathBuf, u64)> = self
            .roots
            .iter()
            .flat_map(|r| r.unattributed.iter().map(|(p, s)| (p, *s)))
            .collect();
        all.sort_by_key(|(_, size)| std::cmp::Reverse(*size));
        all.truncate(limit);
        all
    }
}

/// Walk `roots` and name what can be named. Everything in `closed` is left
/// unopened and reported as such.
pub fn scan_with(roots: &[Root], rules: &[Rule], closed: &[PathBuf]) -> Audit {
    from_sweep(sweep::sweep_gated(roots, rules, closed), rules)
}

/// Sorting happens here rather than in the sweep: a streaming caller needs
/// roots in the order they were announced, and only a finished report wants
/// them biggest-first.
pub fn from_sweep(swept: sweep::Sweep, rules: &[Rule]) -> Audit {
    let mut roots = swept.roots;
    roots.sort_by_key(|r| std::cmp::Reverse(r.total));
    Audit {
        roots,
        categories: group(rules, &swept.rule_sizes),
    }
}

pub fn attribution_rules(closed: &[PathBuf], projects: &[Project]) -> Vec<Rule> {
    let mut rules = system_roots();
    rules.extend(
        Places::current()
            .map(|places| conventions::rules(&places, closed))
            .unwrap_or_default(),
    );
    rules.extend(
        crate::projects::repositories(
            &crate::projects::default_roots(),
            crate::projects::MAX_DEPTH,
            closed,
        )
        .into_iter()
        .map(conventions::source_code_rule),
    );
    rules.extend(projects.iter().flat_map(|project| {
        project
            .artifacts
            .iter()
            .map(|artifact| conventions::build_output_rule(&project.root, artifact.path.clone()))
    }));
    rules
}

const USER_ROOTS: [&str; 3] = ["Home", "User Library", "Other users"];

fn system_roots() -> Vec<Rule> {
    sweep::discover_roots()
        .into_iter()
        .filter(|root| !USER_ROOTS.contains(&root.name.as_str()))
        .map(|root| conventions::system_rule(root.name, root.path))
        .collect()
}

/// Roll per-rule bytes back up into categories, largest first. Empty paths and
/// empty categories are dropped: a rule that matched nothing is noise, and
/// unlike the old code this no longer loses anything, because the bytes it
/// would have hidden are already counted in the root totals.
fn group(rules: &[Rule], sizes: &[u64]) -> Vec<Category> {
    let mut categories: Vec<Category> = Vec::new();

    for (rule, &size) in rules.iter().zip(sizes) {
        if size == 0 {
            continue;
        }
        let entry = CategoryPath {
            label: rule.label.clone(),
            path: rule.path.clone(),
            size,
        };
        match categories.iter_mut().find(|c| c.name == rule.category) {
            Some(existing) => {
                existing.total_size += size;
                existing.paths.push(entry);
            }
            None => categories.push(Category {
                name: rule.category.clone(),
                total_size: size,
                paths: vec![entry],
            }),
        }
    }

    for category in categories.iter_mut() {
        category.paths.sort_by_key(|p| std::cmp::Reverse(p.size));
    }
    categories.sort_by_key(|c| std::cmp::Reverse(c.total_size));
    categories
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn categories_and_unattributed_together_equal_what_was_measured() {
        // The invariant the old audit could not hold: "total tracked" was a
        // sum of an allowlist, and nothing tied it to the disk it described.
        let dir = tempdir().unwrap();
        let named = dir.path().join("named");
        let stray = dir.path().join("stray");
        fs::create_dir_all(&named).unwrap();
        fs::create_dir_all(&stray).unwrap();
        fs::write(named.join("a.bin"), vec![0u8; 128 * 1024]).unwrap();
        fs::write(stray.join("b.bin"), vec![0u8; 256 * 1024]).unwrap();

        let roots = vec![Root {
            name: "test".into(),
            path: dir.path().to_path_buf(),
        }];
        let rules = vec![Rule {
            category: "Named".into(),
            label: "named".into(),
            path: named.clone(),
        }];

        let audit = scan_with(&roots, &rules, &[]);

        assert_eq!(audit.measured(), audit.attributed() + audit.unattributed());
        assert_eq!(audit.categories.len(), 1);
        assert_eq!(audit.categories[0].name, "Named");
    }

    #[test]
    fn the_biggest_unnamed_directory_is_named_in_the_gap() {
        let dir = tempdir().unwrap();
        let stray = dir.path().join("nobody-listed-this");
        fs::create_dir_all(&stray).unwrap();
        fs::write(stray.join("big.bin"), vec![0u8; 512 * 1024]).unwrap();

        let roots = vec![Root {
            name: "test".into(),
            path: dir.path().to_path_buf(),
        }];
        let audit = scan_with(&roots, &[], &[]);

        let largest = audit.largest_unattributed(3);
        assert_eq!(largest[0].0, &stray);
        assert!(largest[0].1 >= 512 * 1024);
    }

    #[test]
    fn a_scan_leaves_closed_directories_unopened_and_counts_them_unattended() {
        let dir = tempdir().unwrap();
        let gated = dir.path().join("Documents");
        fs::create_dir_all(&gated).unwrap();
        fs::write(gated.join("secret.bin"), vec![0u8; 512 * 1024]).unwrap();

        let roots = vec![Root {
            name: "test".into(),
            path: dir.path().to_path_buf(),
        }];
        let audit = scan_with(&roots, &[], std::slice::from_ref(&gated));

        assert!(audit.measured() < 512 * 1024, "{}", audit.measured());
        assert_eq!(audit.denied(Denial::Unattended), 1);
    }

    #[test]
    fn paths_sharing_a_category_are_summed_into_it() {
        let dir = tempdir().unwrap();
        let one = dir.path().join("one");
        let two = dir.path().join("two");
        fs::create_dir_all(&one).unwrap();
        fs::create_dir_all(&two).unwrap();
        fs::write(one.join("a.bin"), vec![0u8; 64 * 1024]).unwrap();
        fs::write(two.join("b.bin"), vec![0u8; 128 * 1024]).unwrap();

        let roots = vec![Root {
            name: "test".into(),
            path: dir.path().to_path_buf(),
        }];
        let rules = vec![
            Rule {
                category: "Tools".into(),
                label: "one".into(),
                path: one,
            },
            Rule {
                category: "Tools".into(),
                label: "two".into(),
                path: two,
            },
        ];

        let audit = scan_with(&roots, &rules, &[]);

        assert_eq!(audit.categories.len(), 1);
        assert_eq!(audit.categories[0].paths.len(), 2);
        assert_eq!(audit.categories[0].paths[0].label, "two", "not sorted");
        assert!(audit.categories[0].total_size >= 192 * 1024);
    }

    #[test]
    fn rules_that_matched_nothing_are_left_out() {
        let dir = tempdir().unwrap();
        let roots = vec![Root {
            name: "test".into(),
            path: dir.path().to_path_buf(),
        }];
        let rules = vec![Rule {
            category: "Ghost".into(),
            label: "missing".into(),
            path: dir.path().join("does-not-exist"),
        }];

        let audit = scan_with(&roots, &rules, &[]);
        assert!(audit.categories.is_empty());
    }
}
