use std::any::TypeId;

#[test]
fn reexports_canonical_types() {
    assert_eq!(
        TypeId::of::<posthog_rs::Event>(),
        TypeId::of::<posthog::Event>()
    );
}
