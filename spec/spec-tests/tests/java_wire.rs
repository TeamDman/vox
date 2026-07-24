#[path = "cases/testbed.rs"]
mod testbed;

use spec_tests::harness::{
    SubjectLanguage, SubjectSpec, accept_bidirectional_subject_spec, accept_subject_spec,
    run_async, run_subject_cancel_timeout, run_subject_client_scenario,
};
use std::time::Duration;

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

#[test]
fn committed_java_timeout_cancels_rust_handler() {
    run_subject_cancel_timeout(SubjectSpec::tcp(SubjectLanguage::Java));
}

#[test]
fn invalid_java_payload_fails_before_rust_dispatch() {
    run_subject_client_scenario(SubjectSpec::tcp(SubjectLanguage::Java), "invalid_payload");
}

#[test]
fn application_error_result_round_trips_both_directions() {
    run_subject_client_scenario(SubjectSpec::tcp(SubjectLanguage::Java), "divide_error");
    testbed::run_rpc_user_error_roundtrip(SubjectSpec::tcp(SubjectLanguage::Java));
}

#[test]
fn disconnect_completes_pending_call_and_java_subject_exits() {
    run_async(async {
        let (client, mut child, connection) =
            accept_subject_spec(SubjectSpec::tcp(SubjectLanguage::Java)).await?;
        {
            let pending = client.echo("disconnect-pending".to_string());
            tokio::pin!(pending);
            tokio::time::sleep(Duration::from_millis(100)).await;
            connection
                .shutdown()
                .map_err(|error| format!("connection shutdown: {error:?}"))?;
            match tokio::time::timeout(Duration::from_secs(2), &mut pending).await {
                Ok(Err(_)) => {}
                Ok(Ok(value)) => {
                    return Err(format!("pending call unexpectedly returned {value:?}"));
                }
                Err(_) => {
                    return Err("pending call remained stranded after disconnect".to_string());
                }
            }
        }
        drop(client);
        drop(connection);
        match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
            Ok(Ok(status)) if status.success() => Ok(()),
            Ok(Ok(status)) => Err(format!("Java subject exited with {status}")),
            Ok(Err(error)) => Err(format!("waiting for Java subject: {error}")),
            Err(_) => {
                let _ = child.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
                Err("Java subject remained alive after disconnect".to_string())
            }
        }
    })
    .unwrap();
}
