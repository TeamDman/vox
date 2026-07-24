#[path = "cases/testbed.rs"]
mod testbed;

use spec_tests::harness::{
    SubjectLanguage, SubjectSpec, accept_bidirectional_subject_spec, accept_subject_spec, run_async,
};

#[test]
fn java_subject_calls_rust_echo_over_tcp() {
    testbed::run_subject_calls_echo(SubjectSpec::tcp(SubjectLanguage::Java));
}

#[test]
fn java_and_rust_echo_on_the_same_connection() {
    run_async(async {
        let (client, mut child, _connection) =
            accept_bidirectional_subject_spec(SubjectSpec::tcp(SubjectLanguage::Java)).await?;
        let response = client
            .echo("rust to java on shared connection".to_string())
            .await
            .map_err(|error| format!("echo: {error:?}"))?;
        if response != "rust to java on shared connection" {
            return Err(format!("unexpected response {response:?}"));
        }
        child.kill().await.ok();
        Ok::<_, String>(())
    })
    .unwrap();
}

#[test]
fn rust_calls_java_subject_echo_over_tcp() {
    run_async(async {
        let (client, mut child, _connection) =
            accept_subject_spec(SubjectSpec::tcp(SubjectLanguage::Java)).await?;
        for message in ["hello", "hello again"] {
            let response = client
                .echo(message.to_string())
                .await
                .map_err(|error| format!("echo: {error:?}"))?;
            if response != message {
                return Err(format!("expected {message:?}, got {response:?}"));
            }
        }
        child.kill().await.ok();
        Ok::<_, String>(())
    })
    .unwrap();
}
