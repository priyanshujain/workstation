//! Profiles command implementation

use console::style;
use ws_dsl::Workstation;

/// Run the profiles command
pub fn run(workstation: &Workstation) -> anyhow::Result<()> {
    let profiles = workstation.profile_names();
    let scopes = workstation.scope_names();

    println!(
        "\n{} Workstation: {}\n",
        style("→").cyan(),
        style(&workstation.name).bold()
    );

    // Show scopes
    println!("{}:", style("Scopes").underlined());
    if scopes.is_empty() {
        println!("  {}", style("(none)").dim());
    } else {
        for scope in &scopes {
            println!("  {} {}", style("•").dim(), scope);
        }
    }

    println!();

    // Show profiles
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
        "{} Use 'ws apply --profile <name>' to apply a profile",
        style("i").blue()
    );

    Ok(())
}
