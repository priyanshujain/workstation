use std::time::Instant;

use anyhow::Result;
use console::style;
use disk::conventions::{Kind, Recovery};
use disk::overview::{disk_overview, other_volumes};
use disk::projects;
use disk::report::{self, Source};
use disk::summary;
use disk::util::format_size;

pub fn summary(no_cache: bool) -> Result<()> {
    let overview = disk_overview();
    let (report, source) =
        report::load_or_refresh(&projects::default_roots(), projects::MAX_DEPTH, no_cache);
    let audit = report.to_audit();
    let home = dirs::home_dir();
    let mut lines = summary::lines(&audit, overview.as_ref(), home.as_deref());
    summary::split_volumes(&mut lines, &other_volumes());
    summary::split_nix_garbage(&mut lines, report.nix_garbage);

    print!("{}", summary::render(&lines, overview.as_ref()));
    if let Some(line) = super::agents::disk_line() {
        println!("{}", style(line).yellow());
    }
    if let Source::Cache { age } = source {
        println!(
            "{}",
            style(format!(
                "measured {}, `wsctl disk --no-cache` to rescan",
                report::humanize_age(age)
            ))
            .dim()
        );
    }
    Ok(())
}

pub fn list(kind: Option<&str>, no_cache: bool) -> Result<()> {
    let (report, _) =
        report::load_or_refresh(&projects::default_roots(), projects::MAX_DEPTH, no_cache);
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let mut rows: Vec<_> = report
        .to_categories()
        .into_iter()
        .filter(|c| kind.is_none_or(|k| c.name.eq_ignore_ascii_case(k)))
        .flat_map(|c| {
            let recovery = Kind::from_label(&c.name).map_or(Recovery::Unknown, Kind::recovery);
            c.paths
                .into_iter()
                .map(move |p| (c.name.clone(), recovery, p))
        })
        .collect();
    rows.sort_by_key(|(_, _, p)| std::cmp::Reverse(p.size));
    println!(
        "{:>9}  {:<7}  {:<15}  {:>6}  {:>8}  {:>7}  path",
        "size", "class", "kind", "used", "modified", "created"
    );
    for (kind, recovery, path) in rows {
        println!(
            "{:>9}  {:<7}  {:<15}  {:>6}  {:>8}  {:>7}  {}",
            format_size(path.size),
            recovery.label(),
            kind,
            ago(now, path.times.accessed),
            ago(now, path.times.modified),
            ago(now, path.times.created),
            path.path.display()
        );
    }
    Ok(())
}

fn ago(now: i64, then: Option<i64>) -> String {
    let Some(then) = then else {
        return "-".into();
    };
    let days = (now - then).max(0) / 86_400;
    match days {
        0 => "today".into(),
        1..=59 => format!("{days}d"),
        60..=729 => format!("{}mo", days / 30),
        _ => format!("{}y", days / 365),
    }
}

pub fn refresh() -> Result<()> {
    let started = Instant::now();
    let report = report::refresh_unattended(&projects::default_roots(), projects::MAX_DEPTH);
    let audit = report.to_audit();
    let denials = audit.denials();
    println!(
        "{} refreshed in {}s: {} measured, {} left closed, {} refused",
        stamp(),
        started.elapsed().as_secs(),
        format_size(audit.measured()),
        denials.unattended,
        denials.protected + denials.forbidden,
    );
    Ok(())
}

fn stamp() -> String {
    let format =
        time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second]Z");
    time::OffsetDateTime::now_utc()
        .format(&format)
        .unwrap_or_default()
}
