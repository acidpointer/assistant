//! Dispatch validates selection, strict types, and arithmetic before effects.
use super::builtin::Calculator;
use super::*;

#[tokio::test]
async fn registry_checks_selection_arguments_and_domain_errors() {
    let mut registry = ToolRegistry::new();
    registry.register(Calculator).expect("register calculator");
    assert!(
        registry.register(Calculator).is_err(),
        "duplicate tool rejected"
    );
    assert!(registry.select(&["missing".into()]).is_err());
    let context = || ToolContext {
        invocation_id: "test".into(),
        cancellation: Cancellation::default(),
    };
    let call = |arguments| ToolCall {
        id: "test".into(),
        name: "calculate".into(),
        arguments,
    };
    let valid = json!({"operation":"multiply","a":19,"b":23});
    let rejected = registry.execute(call(valid.clone()), &[], context()).await;
    assert_eq!(rejected.result["error"]["kind"], "not_allowed");
    let allowed = vec!["calculate".into()];
    let result = registry.execute(call(valid), &allowed, context()).await;
    assert_eq!(result.result["result"]["value"], 437);
    let result = registry
        .execute(
            call(json!({"operation":"add","a":1,"b":2,"unexpected":true})),
            &allowed,
            context(),
        )
        .await;
    assert_eq!(result.result["error"]["kind"], "invalid_arguments");
    let result = registry
        .execute(
            call(json!({"operation":"divide","a":1,"b":0})),
            &allowed,
            context(),
        )
        .await;
    assert_eq!(result.result["error"]["kind"], "tool_failed");
    let schema = &registry.definitions()[0].parameters;
    assert_eq!(schema["additionalProperties"], false);
}
