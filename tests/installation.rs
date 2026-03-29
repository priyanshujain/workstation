use wsctl_core::{Context, Executor, Resource, ResourceGraph};
use ws_macos::BrewFormula;

const TEST_FORMULAE: &[&str] = &["tree", "cowsay"];

fn is_enabled() -> bool {
    std::env::var("WS_INSTALLATION_TEST").unwrap_or_default() == "1"
}

fn cleanup() {
    let ctx = Context::new("test");
    for name in TEST_FORMULAE {
        let _ = ctx.run_command("brew", &["uninstall", "--force", name]);
    }
}

#[test]
fn brew_formula_install_lifecycle() {
    if !is_enabled() {
        eprintln!("skipping installation test (set WS_INSTALLATION_TEST=1 to run)");
        return;
    }

    cleanup();

    let mut graph = ResourceGraph::new();
    for name in TEST_FORMULAE {
        graph.add(BrewFormula::new(*name));
    }
    graph.build_edges().unwrap();

    let ctx = Context::new("test");
    let executor = Executor::new();

    // plan: all packages should need installation
    let plan = executor.plan(&graph, &ctx).expect("failed to plan");
    assert_eq!(plan.creates(), TEST_FORMULAE.len());

    // apply: install them
    let report = executor.execute(plan, &ctx).expect("failed to execute");
    assert!(!report.has_failures(), "failures: {:?}", report.failures());
    assert_eq!(report.success_count(), TEST_FORMULAE.len());

    // verify: all should be present
    for name in TEST_FORMULAE {
        let formula = BrewFormula::new(*name);
        let state = formula.detect(&ctx).expect("detect failed");
        assert!(
            state.is_present(),
            "{name} should be present after install, got {state:?}"
        );
    }

    // re-plan: should have no changes
    let plan = executor.plan(&graph, &ctx).expect("failed to re-plan");
    assert!(plan.is_empty(), "expected no changes, got {}", plan.len());

    // cleanup
    cleanup();

    // verify cleanup
    for name in TEST_FORMULAE {
        let formula = BrewFormula::new(*name);
        let state = formula.detect(&ctx).expect("detect failed after cleanup");
        assert!(
            state.is_absent(),
            "{name} should be absent after cleanup, got {state:?}"
        );
    }
}
