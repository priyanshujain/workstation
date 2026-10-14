use std::collections::HashMap;
use std::path::Path;

use crate::audit::Audit;
use crate::overview::DiskOverview;
use crate::util::format_size;

const OWNERS_SHOWN: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub name: String,
    pub size: u64,
    pub owners: Vec<(String, u64)>,
}

pub fn lines(audit: &Audit, overview: Option<&DiskOverview>, home: Option<&Path>) -> Vec<Line> {
    let mut lines: Vec<Line> = audit
        .categories
        .iter()
        .map(|category| {
            let mut owners: HashMap<&str, u64> = HashMap::new();
            for path in &category.paths {
                *owners.entry(path.label.as_str()).or_default() += path.size;
            }
            Line {
                name: category.name.clone(),
                size: category.total_size,
                owners: biggest(owners.into_iter().map(|(o, s)| (o.to_string(), s))),
            }
        })
        .collect();

    if audit.unattributed() > 0 {
        lines.push(Line {
            name: "Other".into(),
            size: audit.unattributed(),
            owners: biggest(
                audit
                    .largest_unattributed(OWNERS_SHOWN)
                    .into_iter()
                    .map(|(path, size)| (short(path, home), size)),
            ),
        });
    }
    lines.retain(|line| line.size > 0);
    lines.sort_by_key(|line| std::cmp::Reverse(line.size));

    if let Some(overview) = overview {
        let unreached = overview.data_used.saturating_sub(audit.measured());
        if unreached > 0 {
            lines.push(Line {
                name: "Not reached".into(),
                size: unreached,
                owners: Vec::new(),
            });
        }
        if overview.other_volumes > 0 {
            lines.push(Line {
                name: "Other volumes".into(),
                size: overview.other_volumes,
                owners: Vec::new(),
            });
        }
    }
    lines
}

pub fn render(lines: &[Line], overview: Option<&DiskOverview>) -> String {
    let mut out = String::new();
    if let Some(overview) = overview {
        out.push_str(&format!(
            "{} used of {}\n",
            format_size(overview.used()),
            format_size(overview.total)
        ));
    }
    let width = lines.iter().map(|l| l.name.len()).max().unwrap_or(0);
    for line in lines {
        let owners: Vec<String> = line
            .owners
            .iter()
            .map(|(owner, size)| format!("{owner} {}", format_size(*size)))
            .collect();
        out.push_str(
            format!(
                "{:<width$}  {:>9}  {}",
                line.name,
                format_size(line.size),
                owners.join(", ")
            )
            .trim_end(),
        );
        out.push('\n');
    }
    out
}

fn biggest(owners: impl Iterator<Item = (String, u64)>) -> Vec<(String, u64)> {
    let mut owners: Vec<(String, u64)> = owners.filter(|(_, size)| *size > 0).collect();
    owners.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    owners.truncate(OWNERS_SHOWN);
    owners
}

fn short(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{Category, CategoryPath};
    use crate::sweep::{Denials, RootUsage};
    use std::path::PathBuf;

    const GB: u64 = 1_000_000_000;

    fn audit() -> Audit {
        let path = |label: &str, size: u64| CategoryPath {
            label: label.into(),
            path: PathBuf::from(format!("/x/{label}")),
            size,
        };
        Audit {
            roots: vec![RootUsage {
                name: "Home".into(),
                path: "/home/u".into(),
                total: 40 * GB,
                children: Vec::new(),
                refused_children: Vec::new(),
                unattributed_total: 10 * GB,
                unattributed: vec![(PathBuf::from("/home/u/media"), 10 * GB)],
                unreadable: Vec::new(),
                denials: Denials::default(),
            }],
            categories: vec![
                Category {
                    name: "Caches".into(),
                    total_size: 20 * GB,
                    paths: vec![
                        path("tool", 12 * GB),
                        path("tool", 3 * GB),
                        path("app", 5 * GB),
                    ],
                },
                Category {
                    name: "Logs".into(),
                    total_size: 10 * GB,
                    paths: vec![path("app", 10 * GB)],
                },
            ],
        }
    }

    fn overview() -> DiskOverview {
        DiskOverview {
            total: 100 * GB,
            free: 40 * GB,
            data_used: 50 * GB,
            other_volumes: 10 * GB,
        }
    }

    #[test]
    fn kinds_come_biggest_first_with_owners_summed_across_their_folders() {
        let lines = lines(&audit(), None, Some(Path::new("/home/u")));

        assert_eq!(lines[0].name, "Caches");
        assert_eq!(
            lines[0].owners,
            vec![("tool".to_string(), 15 * GB), ("app".to_string(), 5 * GB)]
        );
        assert_eq!(lines[1].name, "Logs");
        assert_eq!(lines[2].name, "Other");
        assert_eq!(lines[2].owners, vec![("~/media".to_string(), 10 * GB)]);
    }

    #[test]
    fn what_the_walk_did_not_reach_and_other_volumes_close_the_total() {
        let overview = overview();
        let lines = lines(&audit(), Some(&overview), None);

        let tail: Vec<(&str, u64)> = lines[3..]
            .iter()
            .map(|l| (l.name.as_str(), l.size))
            .collect();
        assert_eq!(
            tail,
            vec![("Not reached", 10 * GB), ("Other volumes", 10 * GB)]
        );
        let listed: u64 = lines.iter().map(|l| l.size).sum();
        assert_eq!(listed, overview.used());
    }

    #[test]
    fn rendering_is_one_line_per_kind_after_the_headline() {
        let overview = overview();
        let text = render(&lines(&audit(), Some(&overview), None), Some(&overview));
        let rows: Vec<&str> = text.lines().collect();

        assert_eq!(rows.len(), 6);
        assert!(
            rows[0].starts_with(&format!("{} used of", format_size(60 * GB))),
            "{text}"
        );
        assert!(rows[1].starts_with("Caches"), "{text}");
        assert!(
            rows[1].ends_with(&format!(
                "tool {}, app {}",
                format_size(15 * GB),
                format_size(5 * GB)
            )),
            "{text}"
        );
    }
}
