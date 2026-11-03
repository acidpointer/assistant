//! Deterministic stage ordering and service dependencies are build-time contracts.
use super::*;

#[test]
fn hooks_are_ordered_and_cycles_missing_names_and_cross_stage_edges_fail() {
    let mut registrar = PluginRegistrar::default();
    registrar
        .before_run("last", |_| Box::pin(async { Ok(()) }))
        .after("first");
    registrar.before_run("first", |_| Box::pin(async { Ok(()) }));
    let pipeline = registrar.build().expect("valid ordering");
    assert_eq!(
        pipeline.stages(),
        vec![(Stage::BeforeRun, "first"), (Stage::BeforeRun, "last")]
    );
    let mut registrar = PluginRegistrar::default();
    registrar
        .before_run("a", |_| Box::pin(async { Ok(()) }))
        .after("b");
    registrar
        .before_run("b", |_| Box::pin(async { Ok(()) }))
        .after("a");
    assert!(registrar.build().is_err());
    let mut registrar = PluginRegistrar::default();
    registrar
        .before_run("a", |_| Box::pin(async { Ok(()) }))
        .after("missing");
    assert!(registrar.build().is_err());
    let mut registrar = PluginRegistrar::default();
    registrar
        .before_run("a", |_| Box::pin(async { Ok(()) }))
        .after("b");
    registrar.before_model("b", |_, _| Box::pin(async { Ok(()) }));
    assert!(registrar.build().is_err());
}

#[test]
fn services_resolve_after_all_plugins_register() {
    let mut registrar = PluginRegistrar::default();
    registrar.require::<String>();
    assert!(registrar.build().is_err());
    let mut registrar = PluginRegistrar::default();
    registrar.require::<String>();
    registrar
        .provide(Arc::new(String::from("service")))
        .expect("register service");
    assert!(registrar.provide(Arc::new(String::new())).is_err());
    let pipeline = registrar.build().expect("resolve dependency");
    assert_eq!(
        *pipeline.services.get::<String>().expect("typed lookup"),
        "service"
    );
}
