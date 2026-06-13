use super::*;

use pretty_assertions::assert_eq;

#[test]
fn virtualize_child_env_replaces_supported_credentials() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut env = HashMap::from([
        ("GH_TOKEN".to_string(), "ghp-real".to_string()),
        ("OPENAI_API_KEY".to_string(), "sk-real".to_string()),
    ]);

    broker.virtualize_child_env(&mut env);

    assert_eq!(
        env.get("GH_TOKEN"),
        Some(&"ghp_codex_dummy_0000".to_string())
    );
    assert_eq!(
        env.get("OPENAI_API_KEY"),
        Some(&"sk-codex-dummy-0001".to_string())
    );
}

#[test]
fn virtualize_child_env_replaces_unbound_enterprise_token_without_injecting() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut env = HashMap::from([(
        "GH_ENTERPRISE_TOKEN".to_string(),
        "ghp-enterprise-real".to_string(),
    )]);
    let mut headers = HeaderMap::new();

    broker.virtualize_child_env(&mut env);
    broker.inject_request_headers("github.example.com", &mut headers);

    assert_eq!(
        env.get("GH_ENTERPRISE_TOKEN"),
        Some(&"ghp_codex_dummy_0000".to_string())
    );
    assert_eq!(headers.get(AUTHORIZATION), None);
    assert!(!broker.host_requires_mitm("github.example.com"));
}

#[test]
fn inject_request_headers_uses_dummy_to_select_ambiguous_github_credential() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut env = HashMap::from([
        ("GH_TOKEN".to_string(), "ghp-real-one".to_string()),
        ("GITHUB_TOKEN".to_string(), "ghp-real-two".to_string()),
    ]);
    broker.virtualize_child_env(&mut env);
    let github_token = env.get("GITHUB_TOKEN").expect("dummy github token");
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {github_token}")).expect("valid dummy header"),
    );

    broker.inject_request_headers("api.github.com", &mut headers);

    assert_eq!(
        headers.get(AUTHORIZATION),
        Some(&HeaderValue::from_static("Bearer ghp-real-two"))
    );
}

#[test]
fn inject_request_headers_skips_ambiguous_github_credential_without_dummy() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut env = HashMap::from([
        ("GH_TOKEN".to_string(), "ghp-real-one".to_string()),
        ("GITHUB_TOKEN".to_string(), "ghp-real-two".to_string()),
    ]);
    broker.virtualize_child_env(&mut env);
    let mut headers = HeaderMap::new();

    broker.inject_request_headers("api.github.com", &mut headers);

    assert_eq!(headers.get(AUTHORIZATION), None);
}

#[test]
fn inject_request_headers_uses_duplicate_real_github_credential_without_dummy() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut env = HashMap::from([
        ("GH_TOKEN".to_string(), "ghp-real".to_string()),
        ("GITHUB_TOKEN".to_string(), "ghp-real".to_string()),
    ]);
    broker.virtualize_child_env(&mut env);
    let mut headers = HeaderMap::new();

    broker.inject_request_headers("api.github.com", &mut headers);

    assert_eq!(
        headers.get(AUTHORIZATION),
        Some(&HeaderValue::from_static("Bearer ghp-real"))
    );
}

#[test]
fn inject_request_headers_uses_unique_openai_api_key_without_dummy_header() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut env = HashMap::from([("OPENAI_API_KEY".to_string(), "sk-real".to_string())]);
    broker.virtualize_child_env(&mut env);
    let mut headers = HeaderMap::new();

    broker.inject_request_headers("api.openai.com", &mut headers);

    assert_eq!(
        headers.get(AUTHORIZATION),
        Some(&HeaderValue::from_static("Bearer sk-real"))
    );
}

#[test]
fn github_cloud_credentials_match_ghe_com_hosts() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut env = HashMap::from([("GH_TOKEN".to_string(), "ghp-real".to_string())]);
    broker.virtualize_child_env(&mut env);
    let mut headers = HeaderMap::new();

    broker.inject_request_headers("api.astemu.ghe.com", &mut headers);

    assert_eq!(
        headers.get(AUTHORIZATION),
        Some(&HeaderValue::from_static("Bearer ghp-real"))
    );
}

#[test]
fn github_enterprise_credentials_bind_to_gh_host() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut env = HashMap::from([
        ("GH_HOST".to_string(), "github.example.com".to_string()),
        (
            "GH_ENTERPRISE_TOKEN".to_string(),
            "ghp-enterprise-real".to_string(),
        ),
    ]);
    broker.virtualize_child_env(&mut env);
    let mut headers = HeaderMap::new();

    broker.inject_request_headers("github.example.com", &mut headers);

    assert_eq!(
        headers.get(AUTHORIZATION),
        Some(&HeaderValue::from_static("Bearer ghp-enterprise-real"))
    );
    assert!(broker.host_requires_mitm("github.example.com"));
    assert!(!broker.host_requires_mitm("api.github.com"));
}
