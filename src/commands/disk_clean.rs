use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Result, bail};
use console::style;
use disk::cleanup::{CleanAction, Measure, Size, Target, measure};
use disk::conventions::{Kind, Places, Recovery};
use disk::liveness::Liveness;
use disk::offers::{ids, offers};
use disk::util::format_size;
use wsctl_core::privilege::confirm;

struct Item {
    id: String,
    comes_back: &'static str,
    target: Target,
}

pub fn run(names: &[String], dry_run: bool, yes: bool) -> Result<()> {
    let closed = disk::platform::closed_paths();
    let candidates = discover(&closed);
    if names.is_empty() {
        return list(candidates, &closed);
    }
    let chosen = choose(candidates, names, &closed)?;
    clean(chosen, &closed, dry_run, yes)
}

fn discover(closed: &[PathBuf]) -> Vec<Item> {
    let now = SystemTime::now();
    let projects = disk::report::load(now)
        .map(|report| report.to_projects())
        .unwrap_or_default();
    let targets = Places::current()
        .map(|places| offers(&places, closed, &projects, now))
        .unwrap_or_default();
    ids(&targets)
        .into_iter()
        .zip(targets)
        .map(|(id, target)| Item {
            id,
            comes_back: comes_back(&target.name),
            target,
        })
        .collect()
}

fn comes_back(name: &str) -> &'static str {
    let kind = name.split(" > ").next().unwrap_or(name);
    Kind::from_label(kind)
        .map_or(Recovery::Unknown, Kind::recovery)
        .label()
}

fn list(items: Vec<Item>, closed: &[PathBuf]) -> Result<()> {
    if items.is_empty() {
        println!("Nothing to clean.");
        return Ok(());
    }
    let (empty, mut items): (Vec<Item>, Vec<Item>) = measured(items, closed)
        .into_iter()
        .partition(|item| item.target.size().total() == Some(0));
    items.sort_by_key(|item| std::cmp::Reverse(item.target.size().bytes()));
    print_table(&items, &Liveness::snapshot());
    println!();
    if !empty.is_empty() {
        println!("{} empty candidates not shown.", empty.len());
    }
    println!("Clean with: wsctl disk clean <id or path inside one>... --dry-run, then --yes");
    Ok(())
}

fn measured(items: Vec<Item>, closed: &[PathBuf]) -> Vec<Item> {
    eprintln!("Measuring {} item(s)...", items.len());
    let (labels, mut targets): (Vec<_>, Vec<_>) = items
        .into_iter()
        .map(|item| ((item.id, item.comes_back), item.target))
        .unzip();
    measure(&mut targets, closed);
    labels
        .into_iter()
        .zip(targets)
        .map(|((id, comes_back), target)| Item {
            id,
            comes_back,
            target,
        })
        .collect()
}

fn choose(candidates: Vec<Item>, names: &[String], closed: &[PathBuf]) -> Result<Vec<Item>> {
    let mut picked: Vec<usize> = Vec::new();
    let mut inside: Vec<Item> = Vec::new();
    for name in names {
        match candidate_index(&candidates, name) {
            Some(index) if !picked.contains(&index) => picked.push(index),
            Some(_) => {}
            None => inside.push(path_inside(&candidates, name, closed)?),
        }
    }
    let mut chosen: Vec<Item> = candidates
        .into_iter()
        .enumerate()
        .filter(|(index, _)| picked.contains(index))
        .map(|(_, item)| item)
        .collect();
    for item in inside {
        if !covered(&item, &chosen) {
            chosen.push(item);
        }
    }
    Ok(chosen)
}

fn candidate_index(candidates: &[Item], name: &str) -> Option<usize> {
    if let Some(index) = candidates.iter().position(|item| item.id == name) {
        return Some(index);
    }
    let path = Path::new(name).canonicalize().ok()?;
    candidates
        .iter()
        .position(|item| canonical(&item.target).is_some_and(|root| root == path))
}

fn canonical(target: &Target) -> Option<PathBuf> {
    target.path()?.canonicalize().ok()
}

fn drillable(target: &Target) -> bool {
    matches!(
        target.action(),
        CleanAction::RemoveContents(_) | CleanAction::RemoveDir(_)
    )
}

fn path_inside(candidates: &[Item], name: &str, closed: &[PathBuf]) -> Result<Item> {
    let given = Path::new(name);
    if !given.is_absolute() {
        bail!("{name}: no candidate has this id, run `wsctl disk clean` to list them");
    }
    if given.components().any(|c| c == Component::ParentDir) {
        bail!("{name}: give the path without ..");
    }
    if given.is_symlink() {
        bail!("{name}: is a symlink, name what it points to");
    }
    let Ok(path) = given.canonicalize() else {
        bail!("{name}: does not exist");
    };
    let Some(owner) = candidates.iter().find(|item| {
        drillable(&item.target)
            && canonical(&item.target).is_some_and(|root| path.starts_with(root))
    }) else {
        bail!("{name}: not inside any cleanup candidate");
    };
    if disk::sweep::is_closed(&path, closed) {
        bail!("{name}: left closed, needs Full Disk Access");
    }
    let label = path.display().to_string();
    let target = if path.is_dir() {
        Target::pending(
            &label,
            &label,
            Measure::Tree(path.clone()),
            CleanAction::RemoveDir(path),
        )
    } else {
        let size = path.metadata().map(|m| m.len()).unwrap_or(0);
        Target::new(&label, &label, size, CleanAction::RemoveFile(path))
    };
    let target = match owner.target.owner_app() {
        Some(app) => target.owned_by_app(app),
        None => target,
    };
    Ok(Item {
        id: label,
        comes_back: owner.comes_back,
        target,
    })
}

fn covered(item: &Item, chosen: &[Item]) -> bool {
    let Some(path) = item.target.path() else {
        return false;
    };
    chosen.iter().any(|other| {
        canonical(&other.target).is_some_and(|root| {
            path.starts_with(&root) && (drillable(&other.target) || path == root)
        })
    })
}

fn clean(items: Vec<Item>, closed: &[PathBuf], dry_run: bool, yes: bool) -> Result<()> {
    let mut items = measured(items, closed);
    items.sort_by_key(|item| std::cmp::Reverse(item.target.size().bytes()));
    let live = Liveness::snapshot();
    print_table(&items, &live);
    println!();

    let ready: Vec<&Item> = items
        .iter()
        .filter(|item| refusal(&item.target, &live).is_none())
        .collect();
    if ready.is_empty() {
        println!("Nothing to clean: every item named is refused.");
        return Ok(());
    }
    let sizes: Vec<Size> = ready.iter().map(|item| item.target.size()).collect();
    let question = format!("Clean {} item(s), {}?", ready.len(), total_label(&sizes));
    if dry_run {
        println!("Dry run: {question} Nothing changed.");
        return Ok(());
    }
    if !confirm(yes, &question)? {
        println!("Cancelled, nothing changed.");
        return Ok(());
    }

    let failed = ready.iter().filter(|item| !clean_one(item)).count();
    if failed > 0 {
        bail!("{failed} item(s) failed");
    }
    Ok(())
}

fn clean_one(item: &Item) -> bool {
    let target = &item.target;
    let outcome = if target.needs_root() {
        target.clean_as_root()
    } else {
        target.clean()
    };
    match outcome {
        Ok(bytes) => {
            let freed = match target.action() {
                CleanAction::RunCommand(_, _) => "unknown".to_string(),
                _ => format_size(bytes),
            };
            println!("{} {}  freed {freed}", style("cleaned").green(), item.id);
            true
        }
        Err(reason) => {
            println!("{} {}: {reason}", style("failed").red(), item.id);
            false
        }
    }
}

fn refusal(target: &Target, live: &Liveness) -> Option<String> {
    if target.size().denials().unattended > 0 {
        return Some("left closed, needs Full Disk Access".to_string());
    }
    let reason = target.refuse_if_in_use(live).err()?;
    let prefix = format!("{}: ", target.name);
    Some(reason.strip_prefix(&prefix).unwrap_or(&reason).to_string())
}

fn status(target: &Target, live: &Liveness) -> String {
    if let Some(reason) = refusal(target, live) {
        return format!("refused: {reason}");
    }
    let protected = target.size().denials().protected;
    let mut notes = vec!["ready".to_string()];
    if target.needs_root() {
        notes.push("uses sudo".to_string());
    }
    if protected > 0 {
        notes.push(format!("{protected} unreadable, need Full Disk Access"));
    }
    notes.join(", ")
}

fn action_label(action: &CleanAction) -> &'static str {
    match action {
        CleanAction::RemoveContents(_) => "empty",
        CleanAction::RemoveDir(_) => "delete",
        CleanAction::RemoveFile(_) => "delete file",
        CleanAction::RunCommand(_, _) => "run",
        CleanAction::RemoveIdleChildren { .. } => "delete idle",
    }
}

fn size_label(size: Size) -> String {
    match (size.total(), size.bytes()) {
        (Some(bytes), _) => format_size(bytes),
        (None, 0) => "unknown".to_string(),
        (None, bytes) => format!(">= {}", format_size(bytes)),
    }
}

fn total_label(sizes: &[Size]) -> String {
    let bytes: u64 = sizes.iter().map(|size| size.bytes()).sum();
    let complete = sizes.iter().all(|size| size.total().is_some());
    match (complete, bytes) {
        (true, _) => format_size(bytes),
        (false, 0) => "size unknown".to_string(),
        (false, _) => format!("at least {}", format_size(bytes)),
    }
}

fn where_label(target: &Target) -> String {
    match target.action() {
        CleanAction::RunCommand(command, args) => std::iter::once(command)
            .chain(args)
            .cloned()
            .collect::<Vec<_>>()
            .join(" "),
        _ => target.description.clone(),
    }
}

fn print_table(items: &[Item], live: &Liveness) {
    let header = [
        "ID".to_string(),
        "ACTION".to_string(),
        "SIZE".to_string(),
        "COMES BACK".to_string(),
        "STATUS".to_string(),
        "PATH".to_string(),
    ];
    let rows: Vec<[String; 6]> = items
        .iter()
        .map(|item| {
            [
                item.id.clone(),
                action_label(item.target.action()).to_string(),
                size_label(item.target.size()),
                item.comes_back.to_string(),
                status(&item.target, live),
                where_label(&item.target),
            ]
        })
        .collect();
    let widths: Vec<usize> = (0..5)
        .map(|column| {
            std::iter::once(&header)
                .chain(&rows)
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    print_row(&header, &widths, true);
    for row in &rows {
        print_row(row, &widths, false);
    }
}

fn print_row(row: &[String; 6], widths: &[usize], bold: bool) {
    let line = format!(
        "{:<w0$}  {:<w1$}  {:>w2$}  {:<w3$}  {:<w4$}  {}",
        row[0],
        row[1],
        row[2],
        row[3],
        row[4],
        row[5],
        w0 = widths[0],
        w1 = widths[1],
        w2 = widths[2],
        w3 = widths[3],
        w4 = widths[4],
    );
    match bold {
        true => println!("{}", style(line).bold()),
        false => println!("{line}"),
    }
}
