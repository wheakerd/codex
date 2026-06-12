use super::*;

#[test]
fn app_bundled_internal_source_stays_private_on_the_wire() {
    assert_eq!(
        HookSource::from(CoreHookSource::AppBundledInternal),
        HookSource::Plugin
    );
}
