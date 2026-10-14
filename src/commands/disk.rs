use std::time::Instant;

use anyhow::Result;
use console::style;
use disk::overview::disk_overview;
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
    let lines = summary::lines(&audit, overview.as_ref(), home.as_deref());

    print!("{}", summary::render(&lines, overview.as_ref()));
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
