#[path = "cases/testbed.rs"]
mod testbed;

use spec_tests::harness::{SubjectLanguage, SubjectSpec};

#[test]
fn java_subject_calls_rust_echo_over_tcp() {
    testbed::run_subject_calls_echo(SubjectSpec::tcp(SubjectLanguage::Java));
}
