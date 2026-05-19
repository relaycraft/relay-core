#[test]
fn test_backward_compatibility_paths() {
    // 1. relay_core_lib::engine::TcpCaptureSource
    // Verify that we can refer to TcpCaptureSource via the `engine` alias
    // We can't verify TcpCaptureSource easily if it's not re-exported from engine,
    // but we can check the module alias existence.
    // Based on lib.rs: pub use capture::source as engine;
    // capture::source has TcpCaptureSource.
    let _ = std::mem::size_of::<Option<relay_core_lib::engine::TcpCaptureSource>>();

    // 2. relay_core_lib::interceptor::Interceptor
    // Verify Interceptor trait is accessible and NoOpInterceptor is accessible
    let interceptor = relay_core_lib::interceptor::NoOpInterceptor;
    // Check if it implements Interceptor (by assignment to trait object)
    let _: &dyn relay_core_lib::interceptor::Interceptor = &interceptor;

    // 3. relay_core_lib::rule_engine::RuleEngine
    // Verify RuleEngine is accessible via the old path
    let _ = std::mem::size_of::<Option<relay_core_lib::rule_engine::RuleEngine>>();
}
