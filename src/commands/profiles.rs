use crate::builder::Workstation;
use console::style;
use serde::Serialize;

#[derive(Serialize)]
struct ProfilesOutput {
    name: String,
    scopes: Vec<String>,
    profiles: Vec<ProfileEntry>,
}

#[derive(Serialize)]
struct ProfileEntry {
    name: String,
    scopes: Vec<String>,
}

pub fn run(workstation: &Workstation, json: bool) -> anyhow::Result<()> {
    let profiles = workstation.profile_names();
    let scopes = workstation.scope_names();

    if json {
        let entries: Vec<ProfileEntry> = profiles
            .iter()
            .filter_map(|name| {
                workstation
                    .resources
                    .get_profile(name)
                    .map(|p| ProfileEntry {
                        name: p.name.clone(),
                        scopes: p.scopes.clone(),
                    })
            })
            .collect();

        let output = ProfilesOutput {
            name: workstation.name.clone(),
            scopes,
            profiles: entries,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    println!(
        "\n{} Workstation: {}\n",
        style("→").cyan(),
        style(&workstation.name).bold()
    );

    println!("{}:", style("Scopes").underlined());
    if scopes.is_empty() {
        println!("  {}", style("(none)").dim());
    } else {
        for scope in &scopes {
            println!("  {} {}", style("•").dim(), scope);
        }
    }

    println!();

    println!("{}:", style("Profiles").underlined());
    if profiles.is_empty() {
        println!("  {}", style("(none)").dim());
    } else {
        for profile_name in &profiles {
            if let Some(profile) = workstation.resources.get_profile(profile_name) {
                println!(
                    "  {} {} {}",
                    style("•").dim(),
                    style(&profile.name).bold(),
                    style(format!("[{}]", profile.scopes.join(", "))).dim()
                );
            }
        }
    }

    println!();
    println!(
        "{} Use 'wsctl apply <profile>' to apply a profile",
        style("i").blue()
    );

    Ok(())
}
