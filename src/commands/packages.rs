use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::{Result, bail};
use console::style;
use disk::util::format_size;
use packages::brew::info::{self, InstalledPackage, PkgKind};
use packages::brew::ops;
use wsctl_core::privilege::confirm;
use wsctl_core::{CommandRunner, SystemCommandRunner};

use super::apply::{Target, output};

struct Declared {
    brews: BTreeSet<String>,
    casks: BTreeSet<String>,
}

impl Declared {
    fn load(target: &Target) -> Result<Self> {
        Ok(Self {
            brews: declared_names(target, "homebrew.brews")?,
            casks: declared_names(target, "homebrew.casks")?,
        })
    }

    fn option(kind: PkgKind) -> &'static str {
        match kind {
            PkgKind::Formula => "homebrew.brews",
            PkgKind::Cask => "homebrew.casks",
        }
    }

    fn contains(&self, package: &InstalledPackage) -> bool {
        match package.kind {
            PkgKind::Formula => self.brews.contains(&package.name),
            PkgKind::Cask => self.casks.contains(&package.name),
        }
    }
}

struct Inventory {
    packages: Vec<InstalledPackage>,
    declared: Declared,
    dependents: HashMap<String, Vec<String>>,
}

impl Inventory {
    fn load(target: &Target) -> Result<Self> {
        let runner = SystemCommandRunner::new();
        eprintln!("Reading Homebrew packages...");
        let mut packages = info::fetch_installed(&runner)?;
        info::attach_sizes(&info::brew_prefix(&runner)?, &mut packages);
        let declared = Declared::load(target)?;
        let dependents = dependents_map(&packages);
        Ok(Self {
            packages,
            declared,
            dependents,
        })
    }

    fn used_by(&self, name: &str) -> &[String] {
        self.dependents.get(name).map_or(&[], Vec::as_slice)
    }

    fn state(&self, package: &InstalledPackage) -> &'static str {
        if self.declared.contains(package) {
            "declared"
        } else if package.on_request {
            "undeclared"
        } else {
            "dependency"
        }
    }

    fn missing(&self) -> Vec<String> {
        let installed = |kind: PkgKind, name: &String| {
            self.packages
                .iter()
                .any(|package| package.kind == kind && package.name == *name)
        };
        let brews = self
            .declared
            .brews
            .iter()
            .filter(|name| !installed(PkgKind::Formula, name));
        let casks = self
            .declared
            .casks
            .iter()
            .filter(|name| !installed(PkgKind::Cask, name));
        brews.chain(casks).cloned().collect()
    }
}

fn declared_names(target: &Target, option: &str) -> Result<BTreeSet<String>> {
    let attr = format!(
        "{}#darwinConfigurations.{}.config.{option}",
        target.flake, target.host
    );
    let raw = output(
        "nix",
        &["eval", "--json", &attr, "--apply", "map (p: p.name)"],
    )?;
    let names: Vec<String> = serde_json::from_str(&raw)?;
    Ok(names
        .iter()
        .map(|name| short_name(name).to_string())
        .collect())
}

fn short_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

fn dependents_map(packages: &[InstalledPackage]) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for package in packages {
        for dep in &package.deps {
            map.entry(dep.clone())
                .or_default()
                .push(package.name.clone());
        }
    }
    map.values_mut().for_each(|names| names.sort());
    map
}

pub fn list(target: &Target) -> Result<()> {
    let inventory = Inventory::load(target)?;
    let mut packages: Vec<&InstalledPackage> = inventory.packages.iter().collect();
    packages.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes).then(a.name.cmp(&b.name)));
    let rows: Vec<Vec<String>> = packages
        .iter()
        .map(|package| {
            vec![
                package.name.clone(),
                kind_label(package.kind).to_string(),
                package.version.clone().unwrap_or_else(|| "-".to_string()),
                size_label(package.size_bytes),
                inventory.state(package).to_string(),
                package
                    .installed_at
                    .map_or_else(|| "-".to_string(), format_date),
                inventory.used_by(&package.name).join(", "),
            ]
        })
        .collect();
    print_table(
        &[
            "NAME",
            "KIND",
            "VERSION",
            "SIZE",
            "STATE",
            "INSTALLED",
            "USED BY",
        ],
        &rows,
    );
    println!();
    let total: u64 = packages.iter().map(|package| package.size_bytes).sum();
    println!("{} package(s), {}.", packages.len(), format_size(total));
    let missing = inventory.missing();
    if !missing.is_empty() {
        println!(
            "Declared but not installed: {} (wsctl apply installs them).",
            missing.join(", ")
        );
    }
    println!("Remove with: wsctl packages remove <name>... --dry-run, then --yes");
    Ok(())
}

pub fn remove(target: &Target, names: &[String], dry_run: bool, yes: bool) -> Result<()> {
    let inventory = Inventory::load(target)?;
    let order = uninstall_order(&choose(&inventory.packages, names)?);
    let removing: HashSet<&str> = order.iter().map(|package| package.name.as_str()).collect();
    let refusals: Vec<Option<String>> = order
        .iter()
        .map(|package| refusal(&inventory, package, &removing))
        .collect();
    print_plan(&order, &refusals);
    println!();
    print_orphans(&orphans(&inventory.packages, &removing));
    if order.iter().any(|package| package.kind == PkgKind::Cask) {
        println!("Casks are removed with --zap, which also deletes their preferences and data.");
    }
    println!();

    let refused = refusals.iter().flatten().count();
    if refused > 0 {
        bail!("{refused} package(s) refused, nothing removed");
    }
    let size: u64 = order.iter().map(|package| package.size_bytes).sum();
    let question = format!(
        "Uninstall {} package(s), {}?",
        order.len(),
        format_size(size)
    );
    if dry_run {
        println!("Dry run: {question} Nothing changed.");
        return Ok(());
    }
    if !confirm(yes, &question)? {
        println!("Cancelled, nothing changed.");
        return Ok(());
    }
    uninstall(&order)
}

fn choose<'a>(
    packages: &'a [InstalledPackage],
    names: &[String],
) -> Result<Vec<&'a InstalledPackage>> {
    let mut chosen: Vec<&InstalledPackage> = Vec::new();
    for name in names {
        let matches: Vec<&InstalledPackage> = packages
            .iter()
            .filter(|package| package.name == short_name(name))
            .collect();
        match matches.as_slice() {
            [] => bail!("{name}: not installed, run `wsctl packages` to list them"),
            [one] if !chosen.contains(one) => chosen.push(one),
            [_] => {}
            _ => bail!("{name}: is both a formula and a cask"),
        }
    }
    Ok(chosen)
}

fn uninstall_order<'a>(chosen: &[&'a InstalledPackage]) -> Vec<&'a InstalledPackage> {
    let mut remaining = chosen.to_vec();
    let mut order = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let next = remaining
            .iter()
            .position(|package| {
                !remaining
                    .iter()
                    .any(|other| other.deps.contains(&package.name))
            })
            .unwrap_or(0);
        order.push(remaining.remove(next));
    }
    order
}

fn refusal(
    inventory: &Inventory,
    package: &InstalledPackage,
    removing: &HashSet<&str>,
) -> Option<String> {
    if inventory.declared.contains(package) {
        return Some(format!(
            "declared in {}, remove it there first",
            Declared::option(package.kind)
        ));
    }
    let users: Vec<&str> = inventory
        .used_by(&package.name)
        .iter()
        .map(String::as_str)
        .filter(|user| !removing.contains(user))
        .collect();
    (!users.is_empty()).then(|| format!("used by {}", users.join(", ")))
}

fn orphans<'a>(
    packages: &'a [InstalledPackage],
    removing: &HashSet<&str>,
) -> Vec<&'a InstalledPackage> {
    let mut kept: Vec<&InstalledPackage> = packages
        .iter()
        .filter(|package| !removing.contains(package.name.as_str()))
        .collect();
    let mut orphaned = Vec::new();
    while let Some(next) = kept.iter().position(|package| {
        !package.on_request && !kept.iter().any(|other| other.deps.contains(&package.name))
    }) {
        orphaned.push(kept.remove(next));
    }
    orphaned
}

fn print_plan(order: &[&InstalledPackage], refusals: &[Option<String>]) {
    let rows: Vec<Vec<String>> = order
        .iter()
        .zip(refusals)
        .enumerate()
        .map(|(step, (package, refusal))| {
            vec![
                (step + 1).to_string(),
                package.name.clone(),
                kind_label(package.kind).to_string(),
                size_label(package.size_bytes),
                refusal.as_ref().map_or_else(
                    || "ready".to_string(),
                    |reason| format!("refused: {reason}"),
                ),
            ]
        })
        .collect();
    print_table(&["STEP", "NAME", "KIND", "SIZE", "STATUS"], &rows);
}

fn print_orphans(orphans: &[&InstalledPackage]) {
    if orphans.is_empty() {
        println!("brew autoremove then finds no orphaned dependencies.");
        return;
    }
    let names: Vec<&str> = orphans
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    let size: u64 = orphans.iter().map(|package| package.size_bytes).sum();
    println!(
        "brew autoremove then removes {} orphaned dependencies, {}: {}",
        orphans.len(),
        format_size(size),
        names.join(", ")
    );
}

fn uninstall(order: &[&InstalledPackage]) -> Result<()> {
    let runner = SystemCommandRunner::new();
    let mut freed = 0;
    let mut failed = 0;
    for package in order {
        match uninstall_one(&runner, package) {
            Ok(()) => {
                freed += package.size_bytes;
                println!(
                    "{} {}  freed {}",
                    style("removed").green(),
                    package.name,
                    format_size(package.size_bytes)
                );
            }
            Err(reason) => {
                failed += 1;
                println!("{} {}: {reason}", style("failed").red(), package.name);
            }
        }
    }
    match ops::autoremove(&runner) {
        Ok(out) => println!("autoremove: {}", autoremove_summary(&out)),
        Err(error) => println!("{} autoremove: {error}", style("failed").red()),
    }
    println!("Freed {}.", format_size(freed));
    if failed > 0 {
        bail!("{failed} package(s) failed");
    }
    Ok(())
}

fn uninstall_one(runner: &dyn CommandRunner, package: &InstalledPackage) -> Result<()> {
    match package.kind {
        PkgKind::Formula => ops::uninstall_formula(runner, &package.name)?,
        PkgKind::Cask => ops::uninstall_cask_zap(runner, &package.name)?,
    }
    if ops::is_installed(runner, &package.name, package.kind).unwrap_or(true) {
        bail!("still present after uninstall");
    }
    Ok(())
}

fn autoremove_summary(stdout: &str) -> String {
    let removed = stdout
        .lines()
        .filter(|line| line.starts_with("Uninstalling "))
        .count();
    match (removed, stdout.trim().lines().next()) {
        (0, None) => "nothing to remove".to_string(),
        (0, Some(first)) => first.to_string(),
        _ => format!("{removed} orphaned dependencies removed"),
    }
}

fn kind_label(kind: PkgKind) -> &'static str {
    match kind {
        PkgKind::Formula => "formula",
        PkgKind::Cask => "cask",
    }
}

fn size_label(bytes: u64) -> String {
    match bytes {
        0 => "-".to_string(),
        _ => format_size(bytes),
    }
}

fn format_date(unix_secs: u64) -> String {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    time::OffsetDateTime::from_unix_timestamp(unix_secs as i64)
        .ok()
        .and_then(|date| date.format(&format).ok())
        .unwrap_or_else(|| "-".to_string())
}

fn print_table(header: &[&str], rows: &[Vec<String>]) {
    let last = header.len() - 1;
    let widths: Vec<usize> = (0..last)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .chain([header[column].len()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let header: Vec<String> = header.iter().map(|cell| cell.to_string()).collect();
    println!("{}", style(table_line(&header, &widths)).bold());
    for row in rows {
        println!("{}", table_line(row, &widths));
    }
}

fn table_line(row: &[String], widths: &[usize]) -> String {
    let cells: Vec<String> = row
        .iter()
        .enumerate()
        .map(|(column, cell)| match widths.get(column) {
            Some(width) => format!("{cell:<width$}"),
            None => cell.clone(),
        })
        .collect();
    cells.join("  ").trim_end().to_string()
}
